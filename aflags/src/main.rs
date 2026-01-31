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
use log::debug;
use std::collections::{HashMap, HashSet};

mod aconfig_storage_source;
mod device_config_source;
use aconfig_storage_source::AconfigStorageSource;
use device_config_source::DeviceConfigSource;

mod load_protos;

use mainline_beta_namespace_config::{get_mainline_beta_namespace_map, MainlineBetaNamespace};

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
    fn list_flags(&self) -> Result<Vec<Flag>>;
    fn override_flag(
        &self,
        namespace: &str,
        qualified_name: &str,
        value: &str,
        immediate: bool,
    ) -> Result<()>;
    fn unset_flag(&self, namespace: &str, qualified_name: &str, immediate: bool) -> Result<()>;
}

struct FlagSourcesProvider<A, B>
where
    A: FlagSource,
    B: FlagSource,
{
    aconfigd_source: A,
    device_config_source: B,
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

fn get_flag<A: FlagSource, B: FlagSource>(
    qualified_name: &str,
    provider: &FlagSourcesProvider<A, B>,
) -> Result<Flag> {
    let flags_binding = provider.aconfigd_source.list_flags()?;
    let flag = flags_binding.iter().find(|f| f.qualified_name() == qualified_name).ok_or(
        anyhow!("no aconfig flag '{qualified_name}'. Does the flag have an .aconfig definition?"),
    )?;
    Ok(flag.clone())
}

fn set_flag<A: FlagSource, B: FlagSource>(
    flag: &Flag,
    value: &str,
    immediate: bool,
    provider: &FlagSourcesProvider<A, B>,
) -> Result<()> {
    ensure!(
        flag.permission == FlagPermission::ReadWrite,
        format!(
            "could not write flag '{}', it is read-only for the current release configuration.",
            flag.qualified_name()
        )
    );

    provider.aconfigd_source.override_flag(
        &flag.namespace,
        &flag.qualified_name(),
        value,
        immediate,
    )?;
    if flag.storage_backend == FlagStorageBackend::DeviceConfig {
        provider.device_config_source.override_flag(
            &flag.namespace,
            &flag.qualified_name(),
            value,
            immediate,
        )?;
    }
    Ok(())
}

fn unset<A: FlagSource, B: FlagSource>(
    flag: &Flag,
    immediate: bool,
    provider: &FlagSourcesProvider<A, B>,
) -> Result<()> {
    provider.aconfigd_source.unset_flag(&flag.namespace, &flag.qualified_name(), immediate)?;
    if flag.storage_backend == FlagStorageBackend::DeviceConfig {
        provider.device_config_source.unset_flag(
            &flag.namespace,
            &flag.qualified_name(),
            immediate,
        )?;
    }
    Ok(())
}

fn check_container(container: &Option<String>) -> Result<()> {
    if let Some(ref c) = container {
        ensure!(
            load_protos::list_containers()?.contains(c),
            format!("container '{}' not found", &c)
        );
    }

    Ok(())
}

fn merge_mainline_beta_flag(aconfigd_flag: Flag, device_config_flag: Flag) -> Flag {
    Flag {
        value: device_config_flag.value,
        storage_backend: device_config_flag.storage_backend,
        value_picked_from: device_config_flag.value_picked_from,
        permission: device_config_flag.permission,
        ..aconfigd_flag
    }
}

fn resolve_flag(
    aconfigd_flag: Option<Flag>,
    device_config_flag: Option<Flag>,
    mainline_beta_namespace: Option<&MainlineBetaNamespace>,
) -> Option<Flag> {
    match (aconfigd_flag, device_config_flag, mainline_beta_namespace) {
        // If we don't have a device_config flag, keep the aconfigd one (if present).
        (f_option, None, _) => f_option,

        // If we have a device_config flag without a mainline beta namespace, discard it and keep
        // the aconfigd one (if present).
        (f_option, Some(_), None) => f_option,

        // If we have a device_config flag with a mainline beta namespace but no aconfigd flag, the
        // device_config flag is a mainline beta flag *from an APK*, so keep it.
        (None, Some(f), Some(_)) => Some(f),

        // If we have flags from both backends with a mainline beta namespace, check the container
        // to tell whether the flag is mainline beta or not:
        (Some(aconfigd_flag), Some(device_config_flag), Some(mainline_beta_namespace)) => {
            if aconfigd_flag.container == mainline_beta_namespace.container {
                // Flag *is* in the mainline container, so it's mainline beta, so device_config is
                // the source of truth. Use the value from device_config but copy some metadata from
                // the aconfigd flag.
                Some(merge_mainline_beta_flag(aconfigd_flag, device_config_flag))
            } else {
                // Flag is *not* in the mainline container, so it's not mainline beta, so aconfigd
                // is the source of truth.
                Some(aconfigd_flag)
            }
        }
    }
}

fn resolve_flags(
    aconfigd_flags: Vec<Flag>,
    device_config_flags: Vec<Flag>,
    mainline_beta_namespaces: HashMap<&str, &MainlineBetaNamespace>,
) -> Vec<Flag> {
    let mut aconfigd_flag_map =
        aconfigd_flags.into_iter().map(|f| (f.qualified_name(), f)).collect::<HashMap<_, _>>();

    let mut device_config_flag_map = device_config_flags
        .into_iter()
        .map(|f| (f.qualified_name(), f))
        .collect::<HashMap<String, Flag>>();

    let mut qualified_names = HashSet::<String>::new();
    aconfigd_flag_map.keys().for_each(|q| {
        qualified_names.insert(q.clone());
    });
    device_config_flag_map.keys().for_each(|q| {
        qualified_names.insert(q.clone());
    });

    let resolved_flags = qualified_names
        .into_iter()
        .filter_map(|q| {
            let aconfigd_flag = aconfigd_flag_map.remove(&q);
            let device_config_flag = device_config_flag_map.remove(&q);
            let mainline_beta_namespace = mainline_beta_namespaces
                .get(match (&aconfigd_flag, &device_config_flag) {
                    (Some(f), _) => f.namespace.as_str(),
                    (_, Some(f)) => f.namespace.as_str(),
                    (None, None) => unreachable!(),
                })
                .copied();

            resolve_flag(aconfigd_flag, device_config_flag, mainline_beta_namespace)
        })
        .collect::<Vec<Flag>>();

    // All flags should have been removed within map above.
    assert!(aconfigd_flag_map.is_empty());
    assert!(device_config_flag_map.is_empty());

    resolved_flags
}

fn list_flags<A: FlagSource, B: FlagSource>(
    container: Option<String>,
    provider: &FlagSourcesProvider<A, B>,
) -> Result<Vec<Flag>> {
    let flags_unfiltered = if aconfig_flags::auto_generated::aflags_list_mainline_beta() {
        resolve_flags(
            provider.aconfigd_source.list_flags()?,
            provider.device_config_source.list_flags()?,
            get_mainline_beta_namespace_map(),
        )
    } else {
        provider.aconfigd_source.list_flags()?
    };

    let flags = (Filter { container }).apply(&flags_unfiltered);

    Ok(flags)
}

fn list<A: FlagSource, B: FlagSource>(
    container: Option<String>,
    provider: &FlagSourcesProvider<A, B>,
) -> Result<String> {
    check_container(&container)?;

    let flags = list_flags(container, provider)?;

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

    if configinfra_framework_flags_rust::aflags_debug_improvements() {
        logger::init(
            logger::Config::default()
                .with_tag_on_device("aflags")
                .with_max_level(log::LevelFilter::Trace),
        );
        debug!("starting aflags commands.");
    }

    let flag_sources_provider = FlagSourcesProvider {
        aconfigd_source: AconfigStorageSource {},
        device_config_source: DeviceConfigSource {},
    };

    let cli = Cli::parse();
    let output = match cli.command {
        Command::List { container } => list(container, &flag_sources_provider)
            .map_err(|err| anyhow!("could not list flags: {err}"))
            .map(Some),
        Command::Enable { qualified_name, immediate } => {
            let flag = get_flag(&qualified_name, &flag_sources_provider)?;
            set_flag(&flag, "true", immediate, &flag_sources_provider).map(|_| None)
        }
        Command::Disable { qualified_name, immediate } => {
            let flag = get_flag(&qualified_name, &flag_sources_provider)?;
            set_flag(&flag, "false", immediate, &flag_sources_provider).map(|_| None)
        }
        Command::Unset { qualified_name, immediate } => {
            let flag = get_flag(&qualified_name, &flag_sources_provider)?;
            unset(&flag, immediate, &flag_sources_provider).map(|_| None)
        }
    };
    match output {
        Ok(Some(text)) => println!("{text}"),
        Ok(None) => (),
        Err(message) => println!("Error: {message}"),
    }

    Ok(())
}

impl From<bool> for FlagValue {
    fn from(value: bool) -> Self {
        if value {
            FlagValue::Enabled
        } else {
            FlagValue::Disabled
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_config_source::execute_device_config_command;
    use crate::device_config_source::parse_device_config_output;
    use rand::Rng;

    struct TestFlagBuilder(Flag);

    const FLAG_SOURCES_PROVIDER: FlagSourcesProvider<AconfigStorageSource, DeviceConfigSource> =
        FlagSourcesProvider {
            aconfigd_source: AconfigStorageSource {},
            device_config_source: DeviceConfigSource {},
        };

    #[allow(dead_code)]
    impl TestFlagBuilder {
        fn new() -> TestFlagBuilder {
            TestFlagBuilder(Flag {
                namespace: "test_namespace".to_string(),
                name: "test_flag".to_string(),
                package: "test_package".to_string(),
                container: "test_container".to_string(),
                value: FlagValue::Disabled,
                staged_value: None,
                permission: FlagPermission::ReadOnly,
                value_picked_from: ValuePickedFrom::Default,
                storage_backend: FlagStorageBackend::Unspecified,
            })
        }

        fn namespace(mut self, namespace: &str) -> Self {
            self.0.namespace = namespace.to_string();
            self
        }

        fn name(mut self, name: &str) -> Self {
            self.0.name = name.to_string();
            self
        }

        fn value(mut self, value: bool) -> Self {
            self.0.value = FlagValue::from(value);
            self
        }

        fn package(mut self, package: &str) -> Self {
            self.0.package = package.to_string();
            self
        }

        fn container(mut self, container: &str) -> Self {
            self.0.container = container.to_string();
            self
        }

        fn staged_value(mut self, staged_value: Option<bool>) -> Self {
            self.0.staged_value = staged_value.map(FlagValue::from);
            self
        }

        fn permission(mut self, permission: FlagPermission) -> Self {
            self.0.permission = permission;
            self
        }

        fn value_picked_from(mut self, value_picked_from: ValuePickedFrom) -> Self {
            self.0.value_picked_from = value_picked_from;
            self
        }

        fn storage_backend(mut self, storage_backend: FlagStorageBackend) -> Self {
            self.0.storage_backend = storage_backend;
            self
        }

        fn build(self) -> Flag {
            self.0
        }
    }

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
        assert!(set_flag(&flag, "false", false, &FLAG_SOURCES_PROVIDER).is_ok());

        let result = execute_device_config_command(&["list", &namespace]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key(&String::from("some_package.some_flag")));

        let result = execute_device_config_command(&["list", "device_config_overrides"]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key(&format!("{namespace}:some_package.some_flag")));

        // test setting mainline beta flag
        flag.storage_backend = FlagStorageBackend::DeviceConfig;
        assert!(set_flag(&flag, "true", false, &FLAG_SOURCES_PROVIDER).is_ok());

        let result = execute_device_config_command(&["list", &namespace]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        let value = flags.get(&String::from("some_package.some_flag")).unwrap();
        assert_eq!(*value, FlagValue::Enabled);

        let result = execute_device_config_command(&["list", "device_config_overrides"]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        let value = flags.get(&format!("{namespace}:some_package.some_flag")).unwrap();
        assert_eq!(*value, FlagValue::Enabled);

        // test unset mainline beta flag
        assert!(unset(&flag, false, &FLAG_SOURCES_PROVIDER).is_ok());

        let result = execute_device_config_command(&["list", &namespace]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key(&String::from("some_package.some_flag")));

        let result = execute_device_config_command(&["list", "device_config_overrides"]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key(&format!("{namespace}:some_package.some_flag")));
    }

    #[test]
    fn test_resolve_flag_aconfigd_without_mainline_beta_namespace() {
        let aconfigd_flag = TestFlagBuilder::new()
            .name("test_aconfigd_flag")
            .storage_backend(FlagStorageBackend::Aconfigd)
            .build();
        let resolved = resolve_flag(Some(aconfigd_flag), None, None);
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.name, "test_aconfigd_flag");
    }

    #[test]
    fn test_resolve_flag_aconfigd_with_mainline_beta_namespace() {
        let aconfigd_flag = TestFlagBuilder::new()
            .name("test_aconfigd_flag")
            .storage_backend(FlagStorageBackend::Aconfigd)
            .build();
        let mainline_beta_namespace =
            MainlineBetaNamespace { container: "test_container", allow_exported: false };
        let resolved = resolve_flag(Some(aconfigd_flag), None, Some(&mainline_beta_namespace));
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.name, "test_aconfigd_flag");
    }

    #[test]
    fn test_resolve_flag_device_config_only_without_mainline_beta_namespace() {
        let device_config_flag =
            TestFlagBuilder::new().storage_backend(FlagStorageBackend::DeviceConfig).build();
        let resolved = resolve_flag(None, Some(device_config_flag), None);
        assert!(resolved.is_none());
    }

    #[test]
    fn test_resolve_flag_device_config_only_with_mainline_beta_namespace() {
        let device_config_flag = TestFlagBuilder::new()
            .name("test_device_config_flag")
            .storage_backend(FlagStorageBackend::DeviceConfig)
            .build();
        let mainline_beta_namespace =
            MainlineBetaNamespace { container: "test_container", allow_exported: false };
        let resolved = resolve_flag(None, Some(device_config_flag), Some(&mainline_beta_namespace));
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.name, "test_device_config_flag");
    }

    #[test]
    fn test_resolve_flag_both_backends_without_mainline_beta_namespace() {
        let aconfigd_flag =
            TestFlagBuilder::new().storage_backend(FlagStorageBackend::Aconfigd).build();
        let device_config_flag =
            TestFlagBuilder::new().storage_backend(FlagStorageBackend::DeviceConfig).build();
        let resolved = resolve_flag(Some(aconfigd_flag), Some(device_config_flag), None);
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.storage_backend, FlagStorageBackend::Aconfigd);
    }

    #[test]
    fn test_resolve_flag_both_backends_with_mainline_beta_namespace_in_platform_container() {
        let aconfigd_flag = TestFlagBuilder::new()
            .storage_backend(FlagStorageBackend::Aconfigd)
            .container("platform_container")
            .build();
        let device_config_flag =
            TestFlagBuilder::new().storage_backend(FlagStorageBackend::DeviceConfig).build();
        let mainline_beta_namespace =
            MainlineBetaNamespace { container: "mainline_container", allow_exported: false };
        let resolved = resolve_flag(
            Some(aconfigd_flag),
            Some(device_config_flag),
            Some(&mainline_beta_namespace),
        );
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.storage_backend, FlagStorageBackend::Aconfigd);
    }

    #[test]
    fn test_resolve_flag_both_backends_with_mainline_beta_namespace_in_mainline_container() {
        let aconfigd_flag = TestFlagBuilder::new()
            .storage_backend(FlagStorageBackend::Aconfigd)
            .container("mainline_container")
            .build();
        let device_config_flag =
            TestFlagBuilder::new().storage_backend(FlagStorageBackend::DeviceConfig).build();
        let mainline_beta_namespace =
            MainlineBetaNamespace { container: "mainline_container", allow_exported: false };
        let resolved = resolve_flag(
            Some(aconfigd_flag),
            Some(device_config_flag),
            Some(&mainline_beta_namespace),
        );
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.storage_backend, FlagStorageBackend::DeviceConfig);
    }
}
