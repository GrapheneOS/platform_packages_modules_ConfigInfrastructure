/*
 * Copyright (C) 2024 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! `aflags` is a device binary to read and write aconfig flags.

use anyhow::{anyhow, ensure, Result};
use clap::Parser;

mod aconfig_storage_source;
mod device_config_source;
use aconfig_storage_source::AconfigStorageSource;
use device_config_source::DeviceConfigSource;

mod load_protos;

#[derive(Clone, PartialEq, Debug)]
enum FlagPermission {
    ReadOnly,
    ReadWrite,
}

impl std::fmt::Display for FlagPermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match &self {
                Self::ReadOnly => "read-only",
                Self::ReadWrite => "read-write",
            }
        )
    }
}

#[derive(Clone, Debug)]
enum ValuePickedFrom {
    Default,
    Server,
    Local,
}

#[derive(Clone, Debug, PartialEq)]
enum FlagStorageBackend {
    Unspecified,
    None,
    Aconfigd,
    DeviceConfig,
}

impl std::fmt::Display for ValuePickedFrom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match &self {
                Self::Default => "default",
                Self::Server => "server",
                Self::Local => "local",
            }
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FlagValue {
    Enabled,
    Disabled,
}

impl TryFrom<&str> for FlagValue {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> std::result::Result<Self, Self::Error> {
        match value {
            "true" | "enabled" => Ok(Self::Enabled),
            "false" | "disabled" => Ok(Self::Disabled),
            _ => Err(anyhow!("cannot convert string '{}' to FlagValue", value)),
        }
    }
}

impl std::fmt::Display for FlagValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match &self {
                Self::Enabled => "enabled",
                Self::Disabled => "disabled",
            }
        )
    }
}

#[derive(Clone, Debug)]
struct Flag {
    namespace: String,
    name: String,
    package: String,
    container: String,
    value: FlagValue,
    staged_value: Option<FlagValue>,
    permission: FlagPermission,
    value_picked_from: ValuePickedFrom,
    storage_backend: FlagStorageBackend,
}

impl Flag {
    fn qualified_name(&self) -> String {
        format!("{}.{}", self.package, self.name)
    }

    fn display_staged_value(&self) -> String {
        match (&self.permission, self.staged_value) {
            (FlagPermission::ReadOnly, _) => "-".to_string(),
            (FlagPermission::ReadWrite, None) => "-".to_string(),
            (FlagPermission::ReadWrite, Some(v)) => format!("(->{v})"),
        }
    }
}

trait FlagSource {
    fn list_flags() -> Result<Vec<Flag>>;
    fn override_flag(
        namespace: &str,
        qualified_name: &str,
        value: &str,
        immediate: bool,
    ) -> Result<()>;
    fn unset_flag(namespace: &str, qualified_name: &str, immediate: bool) -> Result<()>;
}

const ABOUT_TEXT: &str = "Tool for reading and writing flags.

Rows in the table from the `list` command follow this format:

  package flag_name value provenance permission container

  * `package`: package set for this flag in its .aconfig definition.
  * `flag_name`: flag name, also set in definition.
  * `value`: the value read from the flag.
  * `staged_value`: the value on next boot:
    + `-`: same as current value
    + `(->enabled) flipped to enabled on boot.
    + `(->disabled) flipped to disabled on boot.
  * `provenance`: one of:
    + `default`: the flag value comes from its build-time default.
    + `server`: the flag value comes from a server override.
    + `local`: the flag value comes from local override.
  * `permission`: read-write or read-only.
  * `container`: the container for the flag, configured in its definition.
";

#[derive(Parser, Debug)]
#[clap(long_about=ABOUT_TEXT, bin_name="aflags")]
struct Cli {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Parser, Debug)]
enum Command {
    /// List all aconfig flags on this device.
    List {
        /// Optionally filter by container name.
        #[clap(short = 'c', long = "container")]
        container: Option<String>,
    },

    /// Locally enable an aconfig flag on this device.
    ///
    /// Prevents server overrides until the value is unset.
    ///
    /// By default, requires a reboot to take effect.
    Enable {
        /// <package>.<flag_name>
        qualified_name: String,

        /// Apply the change immediately.
        #[clap(short = 'i', long = "immediate")]
        immediate: bool,
    },

    /// Locally disable an aconfig flag on this device.
    ///
    /// Prevents server overrides until the value is unset.
    ///
    /// By default, requires a reboot to take effect.
    Disable {
        /// <package>.<flag_name>
        qualified_name: String,

        /// Apply the change immediately.
        #[clap(short = 'i', long = "immediate")]
        immediate: bool,
    },

    /// Clear any local override value and re-allow server overrides.
    ///
    /// By default, requires a reboot to take effect.
    Unset {
        /// <package>.<flag_name>
        qualified_name: String,

        /// Apply the change immediately.
        #[clap(short = 'i', long = "immediate")]
        immediate: bool,
    },
}

struct PaddingInfo {
    longest_flag_col: usize,
    longest_val_col: usize,
    longest_staged_val_col: usize,
    longest_value_picked_from_col: usize,
    longest_permission_col: usize,
}

struct Filter {
    container: Option<String>,
}

impl Filter {
    fn apply(&self, flags: &[Flag]) -> Vec<Flag> {
        flags
            .iter()
            .filter(|flag| match &self.container {
                Some(c) => flag.container == *c,
                None => true,
            })
            .cloned()
            .collect()
    }
}

fn format_flag_row(flag: &Flag, info: &PaddingInfo) -> String {
    let full_name = flag.qualified_name();
    let p0 = info.longest_flag_col + 1;

    let val = flag.value.to_string();
    let p1 = info.longest_val_col + 1;

    let staged_val = flag.display_staged_value();
    let p2 = info.longest_staged_val_col + 1;

    let value_picked_from = flag.value_picked_from.to_string();
    let p3 = info.longest_value_picked_from_col + 1;

    let perm = flag.permission.to_string();
    let p4 = info.longest_permission_col + 1;

    let container = &flag.container;

    format!(
        "{full_name:p0$}{val:p1$}{staged_val:p2$}{value_picked_from:p3$}{perm:p4$}{container}\n"
    )
}

fn get_flag(qualified_name: &str) -> Result<Flag> {
    let flags_binding = AconfigStorageSource::list_flags()?;
    let flag = flags_binding.iter().find(|f| f.qualified_name() == qualified_name).ok_or(
        anyhow!("no aconfig flag '{qualified_name}'. Does the flag have an .aconfig definition?"),
    )?;
    Ok(flag.clone())
}

fn set_flag(flag: &Flag, value: &str, immediate: bool) -> Result<()> {
    ensure!(
        flag.permission == FlagPermission::ReadWrite,
        format!(
            "could not write flag '{}', it is read-only for the current release configuration.",
            flag.qualified_name()
        )
    );

    AconfigStorageSource::override_flag(&flag.namespace, &flag.qualified_name(), value, immediate)?;
    if flag.storage_backend == FlagStorageBackend::DeviceConfig {
        DeviceConfigSource::override_flag(
            &flag.namespace,
            &flag.qualified_name(),
            value,
            immediate,
        )?;
    }
    Ok(())
}

fn unset(flag: &Flag, immediate: bool) -> Result<()> {
    AconfigStorageSource::unset_flag(&flag.namespace, &flag.qualified_name(), immediate)?;
    if flag.storage_backend == FlagStorageBackend::DeviceConfig {
        DeviceConfigSource::unset_flag(&flag.namespace, &flag.qualified_name(), immediate)?;
    }
    Ok(())
}

fn list(container: Option<String>) -> Result<String> {
    let flags_unfiltered = AconfigStorageSource::list_flags()?;

    if let Some(ref c) = container {
        ensure!(
            load_protos::list_containers()?.contains(c),
            format!("container '{}' not found", &c)
        );
    }

    let flags = (Filter { container }).apply(&flags_unfiltered);
    let padding_info = PaddingInfo {
        longest_flag_col: flags.iter().map(|f| f.qualified_name().len()).max().unwrap_or(0),
        longest_val_col: flags.iter().map(|f| f.value.to_string().len()).max().unwrap_or(0),
        longest_staged_val_col: flags
            .iter()
            .map(|f| f.display_staged_value().len())
            .max()
            .unwrap_or(0),
        longest_value_picked_from_col: flags
            .iter()
            .map(|f| f.value_picked_from.to_string().len())
            .max()
            .unwrap_or(0),
        longest_permission_col: flags
            .iter()
            .map(|f| f.permission.to_string().len())
            .max()
            .unwrap_or(0),
    };

    let mut result = String::from("");
    for flag in flags {
        let row = format_flag_row(&flag, &padding_info);
        result.push_str(&row);
    }
    Ok(result)
}

fn main() -> Result<()> {
    ensure!(nix::unistd::Uid::current().is_root(), "must be root");

    let cli = Cli::parse();
    let output = match cli.command {
        Command::List { container } => {
            list(container).map_err(|err| anyhow!("could not list flags: {err}")).map(Some)
        }
        Command::Enable { qualified_name, immediate } => {
            let flag = get_flag(&qualified_name)?;
            set_flag(&flag, "true", immediate).map(|_| None)
        }
        Command::Disable { qualified_name, immediate } => {
            let flag = get_flag(&qualified_name)?;
            set_flag(&flag, "false", immediate).map(|_| None)
        }
        Command::Unset { qualified_name, immediate } => {
            let flag = get_flag(&qualified_name)?;
            unset(&flag, immediate).map(|_| None)
        }
    };
    match output {
        Ok(Some(text)) => println!("{text}"),
        Ok(None) => (),
        Err(message) => println!("Error: {message}"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_config_source::execute_device_config_command;
    use crate::device_config_source::parse_device_config_output;
    use rand::Rng;

    #[test]
    fn test_filter_container() {
        let flags = vec![
            Flag {
                namespace: "namespace".to_string(),
                name: "test1".to_string(),
                package: "package".to_string(),
                value: FlagValue::Disabled,
                staged_value: None,
                permission: FlagPermission::ReadWrite,
                value_picked_from: ValuePickedFrom::Default,
                container: "system".to_string(),
                storage_backend: FlagStorageBackend::Aconfigd,
            },
            Flag {
                namespace: "namespace".to_string(),
                name: "test2".to_string(),
                package: "package".to_string(),
                value: FlagValue::Disabled,
                staged_value: None,
                permission: FlagPermission::ReadWrite,
                value_picked_from: ValuePickedFrom::Default,
                container: "not_system".to_string(),
                storage_backend: FlagStorageBackend::Aconfigd,
            },
            Flag {
                namespace: "namespace".to_string(),
                name: "test3".to_string(),
                package: "package".to_string(),
                value: FlagValue::Disabled,
                staged_value: None,
                permission: FlagPermission::ReadWrite,
                value_picked_from: ValuePickedFrom::Default,
                container: "system".to_string(),
                storage_backend: FlagStorageBackend::Aconfigd,
            },
        ];

        assert_eq!((Filter { container: Some("system".to_string()) }).apply(&flags).len(), 2);
    }

    #[test]
    fn test_filter_no_container() {
        let flags = vec![
            Flag {
                namespace: "namespace".to_string(),
                name: "test1".to_string(),
                package: "package".to_string(),
                value: FlagValue::Disabled,
                staged_value: None,
                permission: FlagPermission::ReadWrite,
                value_picked_from: ValuePickedFrom::Default,
                container: "system".to_string(),
                storage_backend: FlagStorageBackend::Aconfigd,
            },
            Flag {
                namespace: "namespace".to_string(),
                name: "test2".to_string(),
                package: "package".to_string(),
                value: FlagValue::Disabled,
                staged_value: None,
                permission: FlagPermission::ReadWrite,
                value_picked_from: ValuePickedFrom::Default,
                container: "not_system".to_string(),
                storage_backend: FlagStorageBackend::Aconfigd,
            },
            Flag {
                namespace: "namespace".to_string(),
                name: "test3".to_string(),
                package: "package".to_string(),
                value: FlagValue::Disabled,
                staged_value: None,
                permission: FlagPermission::ReadWrite,
                value_picked_from: ValuePickedFrom::Default,
                container: "system".to_string(),
                storage_backend: FlagStorageBackend::Aconfigd,
            },
        ];

        assert_eq!((Filter { container: None }).apply(&flags).len(), 3);
    }

    #[test]
    #[cfg(not(feature = "cargo"))]
    fn test_set_unset_mainline_beta_flag() {
        let mut rng = rand::thread_rng();
        let namespace = rng.gen::<u32>().to_string();
        let mut flag = Flag {
            namespace: namespace.clone(),
            name: String::from("some_flag"),
            package: String::from("some_package"),
            container: String::from("system"),
            value: FlagValue::Disabled,
            staged_value: None,
            permission: FlagPermission::ReadWrite,
            value_picked_from: ValuePickedFrom::Default,
            storage_backend: FlagStorageBackend::Aconfigd,
        };

        // negative test to ensure value is not synced over to device config
        assert!(set_flag(&flag, "false", false).is_ok());

        let result = execute_device_config_command(&["list", &namespace]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key(&String::from("some_package.some_flag")));

        let result = execute_device_config_command(&["list", "device_config_overrides"]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key(&format!("{namespace}:some_package.some_flag")));

        // test setting mainline beta flag
        flag.storage_backend = FlagStorageBackend::DeviceConfig;
        assert!(set_flag(&flag, "true", false).is_ok());

        let result = execute_device_config_command(&["list", &namespace]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        let value = flags.get(&String::from("some_package.some_flag")).unwrap();
        assert_eq!(*value, FlagValue::Enabled);

        let result = execute_device_config_command(&["list", "device_config_overrides"]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        let value = flags.get(&format!("{namespace}:some_package.some_flag")).unwrap();
        assert_eq!(*value, FlagValue::Enabled);

        // test unset mainline beta flag
        assert!(unset(&flag, false).is_ok());

        let result = execute_device_config_command(&["list", &namespace]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key(&String::from("some_package.some_flag")));

        let result = execute_device_config_command(&["list", "device_config_overrides"]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key(&format!("{namespace}:some_package.some_flag")));
    }
}
