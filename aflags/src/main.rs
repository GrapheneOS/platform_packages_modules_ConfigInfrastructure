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

/// The canonical string representation of an enabled boolean flag.
pub const VALUE_ENABLED: &str = "enabled";
/// The canonical string representation of a disabled boolean flag.
pub const VALUE_DISABLED: &str = "disabled";

pub use aflags_protos::ProtoFlag as Flag;
pub use aflags_protos::ProtoFlagList;
pub use aflags_protos::ProtoFlagPermission as FlagPermission;
pub use aflags_protos::ProtoFlagStorageBackend as FlagStorageBackend;
pub use aflags_protos::ProtoValuePickedFrom as ValuePickedFrom;

use anyhow::{anyhow, bail, ensure, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use clap::Parser;
use log::debug;
use protobuf::Message;
use std::collections::{HashMap, HashSet};
use std::io::Write;

mod aconfig_storage_source;
mod device_config_source;
use aconfig_storage_source::AconfigStorageSource;
use device_config_source::DeviceConfigSource;

mod load_protos;

use mainline_beta_namespace_config::{get_mainline_beta_namespace_map, MainlineBetaNamespace};

/// Normalizes a flag value for display in the CLI.
///
/// Maps "true" or "enabled" to "enabled", and "false" or "disabled" to "disabled".
/// Other values are returned as-is.
///
/// b/394883198 will add support for string-type flags.
pub fn flag_value_from_str(value: &str) -> String {
    match value {
        "true" | VALUE_ENABLED => VALUE_ENABLED.to_string(),
        "false" | VALUE_DISABLED => VALUE_DISABLED.to_string(),
        _ => value.to_string(),
    }
}

fn flag_permission_to_string(permission: FlagPermission) -> String {
    match permission {
        FlagPermission::FLAG_PERMISSION_READ_ONLY => "read-only".to_string(),
        FlagPermission::FLAG_PERMISSION_READ_WRITE => "read-write".to_string(),
        FlagPermission::FLAG_PERMISSION_UNSPECIFIED => "unspecified".to_string(),
    }
}

fn value_picked_from_to_string(value_picked_from: ValuePickedFrom) -> String {
    match value_picked_from {
        ValuePickedFrom::VALUE_PICKED_FROM_DEFAULT => "default".to_string(),
        ValuePickedFrom::VALUE_PICKED_FROM_SERVER => "server".to_string(),
        ValuePickedFrom::VALUE_PICKED_FROM_LOCAL => "local".to_string(),
        ValuePickedFrom::VALUE_PICKED_FROM_UNSPECIFIED => "unspecified".to_string(),
    }
}

fn flag_qualified_name(flag: &Flag) -> String {
    format!("{}.{}", flag.package(), flag.name())
}

fn flag_display_staged_value(flag: &Flag) -> String {
    match (flag.permission(), flag.staged_value.as_deref()) {
        (FlagPermission::FLAG_PERMISSION_READ_ONLY, _) => "-".to_string(),
        (FlagPermission::FLAG_PERMISSION_READ_WRITE, None) => "-".to_string(),
        (FlagPermission::FLAG_PERMISSION_READ_WRITE, Some(v)) => {
            format!("(->{v})")
        }
        (FlagPermission::FLAG_PERMISSION_UNSPECIFIED, _) => "-".to_string(),
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

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum OutputFormat {
    Text,
    Proto,
}

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

        /// Output format.
        ///
        /// When using 'proto', the output is Base64 encoded.
        #[clap(short = 'f', long = "format", default_value = "text")]
        format: OutputFormat,
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
                Some(c) => flag.container() == c,
                None => true,
            })
            .cloned()
            .collect()
    }
}

fn format_flag_row(flag: &Flag, info: &PaddingInfo) -> String {
    let full_name = flag_qualified_name(flag);
    let p0 = info.longest_flag_col + 1;

    let val = flag.value();
    let p1 = info.longest_val_col + 1;

    let staged_val = flag_display_staged_value(flag);
    let p2 = info.longest_staged_val_col + 1;

    let value_picked_from = value_picked_from_to_string(flag.value_picked_from());
    let p3 = info.longest_value_picked_from_col + 1;

    let perm = flag_permission_to_string(flag.permission());
    let p4 = info.longest_permission_col + 1;

    let container = flag.container();

    format!(
        "{full_name:p0$}{val:p1$}{staged_val:p2$}{value_picked_from:p3$}{perm:p4$}{container}\n"
    )
}

fn get_flag<A: FlagSource, B: FlagSource>(
    qualified_name: &str,
    provider: &FlagSourcesProvider<A, B>,
) -> Result<Flag> {
    let flags_binding = provider.aconfigd_source.list_flags()?;
    let flag = flags_binding.iter().find(|f| flag_qualified_name(f) == qualified_name).ok_or(
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
        flag.permission() == FlagPermission::FLAG_PERMISSION_READ_WRITE,
        format!(
            "could not write flag '{}', it is read-only for the current release configuration.",
            flag_qualified_name(flag)
        )
    );

    provider.aconfigd_source.override_flag(
        flag.namespace(),
        &flag_qualified_name(flag),
        value,
        immediate,
    )?;
    if flag.storage_backend() == FlagStorageBackend::FLAG_STORAGE_BACKEND_DEVICE_CONFIG {
        provider.device_config_source.override_flag(
            flag.namespace(),
            &flag_qualified_name(flag),
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
    provider.aconfigd_source.unset_flag(flag.namespace(), &flag_qualified_name(flag), immediate)?;
    if flag.storage_backend() == FlagStorageBackend::FLAG_STORAGE_BACKEND_DEVICE_CONFIG {
        provider.device_config_source.unset_flag(
            flag.namespace(),
            &flag_qualified_name(flag),
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

fn merge_mainline_beta_flag(mut aconfigd_flag: Flag, device_config_flag: Flag) -> Flag {
    aconfigd_flag.set_value(device_config_flag.value().to_string());
    aconfigd_flag.set_storage_backend(device_config_flag.storage_backend());
    aconfigd_flag.set_value_picked_from(device_config_flag.value_picked_from());
    aconfigd_flag.set_permission(device_config_flag.permission());
    aconfigd_flag
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
            if aconfigd_flag.container() == mainline_beta_namespace.container {
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
        aconfigd_flags.into_iter().map(|f| (flag_qualified_name(&f), f)).collect::<HashMap<_, _>>();

    let mut device_config_flag_map = device_config_flags
        .into_iter()
        .map(|f| (flag_qualified_name(&f), f))
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
                    (Some(f), _) => f.namespace(),
                    (_, Some(f)) => f.namespace(),
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
    format: OutputFormat,
    provider: &FlagSourcesProvider<A, B>,
) -> Result<Vec<u8>> {
    check_container(&container)?;

    let flags = list_flags(container, provider)?;

    match format {
        OutputFormat::Text => {
            let padding_info = PaddingInfo {
                longest_flag_col: flags
                    .iter()
                    .map(|f| flag_qualified_name(f).len())
                    .max()
                    .unwrap_or(0),
                longest_val_col: flags.iter().map(|f| f.value().len()).max().unwrap_or(0),
                longest_staged_val_col: flags
                    .iter()
                    .map(|f| flag_display_staged_value(f).len())
                    .max()
                    .unwrap_or(0),
                longest_value_picked_from_col: flags
                    .iter()
                    .map(|f| value_picked_from_to_string(f.value_picked_from()).len())
                    .max()
                    .unwrap_or(0),
                longest_permission_col: flags
                    .iter()
                    .map(|f| flag_permission_to_string(f.permission()).len())
                    .max()
                    .unwrap_or(0),
            };

            let mut result = String::from("");
            for flag in flags {
                let row = format_flag_row(&flag, &padding_info);
                result.push_str(&row);
            }
            result.push('\n');
            Ok(result.into_bytes())
        }
        OutputFormat::Proto => {
            if !configinfra_framework_flags_rust::aflags_list_proto() {
                bail!("Protobuf output format is not enabled. Please enable the 'android.provider.flags.aflags_list_proto' flag to use this feature if needed.");
            }
            let mut flag_list = ProtoFlagList::new();
            flag_list.flags = flags;
            let bytes = flag_list.write_to_bytes()?;
            // Base64 encode to prevent binary mangling over adb shell.
            let mut result = BASE64_STANDARD.encode(bytes);
            result.push('\n');
            Ok(result.into_bytes())
        }
    }
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
        Command::List { container, format } => list(container, format, &flag_sources_provider)
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
        Ok(Some(bytes)) => {
            std::io::stdout().write_all(&bytes)?;
        }
        Ok(None) => (),
        Err(message) => println!("Error: {message}"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag_value_from_bool(value: bool) -> String {
        if value { VALUE_ENABLED } else { VALUE_DISABLED }.to_string()
    }

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
            let mut f = Flag::new();
            f.set_namespace("test_namespace".to_string());
            f.set_name("test_flag".to_string());
            f.set_package("test_package".to_string());
            f.set_container("test_container".to_string());
            f.set_value(VALUE_DISABLED.to_string());
            f.set_permission(FlagPermission::FLAG_PERMISSION_READ_ONLY);
            f.set_value_picked_from(ValuePickedFrom::VALUE_PICKED_FROM_DEFAULT);
            f.set_storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_UNSPECIFIED);
            TestFlagBuilder(f)
        }

        fn namespace(mut self, namespace: &str) -> Self {
            self.0.set_namespace(namespace.to_string());
            self
        }

        fn name(mut self, name: &str) -> Self {
            self.0.set_name(name.to_string());
            self
        }

        fn value(mut self, value: bool) -> Self {
            self.0.set_value(flag_value_from_bool(value));
            self
        }

        fn package(mut self, package: &str) -> Self {
            self.0.set_package(package.to_string());
            self
        }

        fn container(mut self, container: &str) -> Self {
            self.0.set_container(container.to_string());
            self
        }

        fn staged_value(mut self, staged_value: Option<bool>) -> Self {
            self.0.staged_value = staged_value.map(flag_value_from_bool);
            self
        }

        fn permission(mut self, permission: FlagPermission) -> Self {
            self.0.set_permission(permission);
            self
        }

        fn value_picked_from(mut self, value_picked_from: ValuePickedFrom) -> Self {
            self.0.set_value_picked_from(value_picked_from);
            self
        }

        fn storage_backend(mut self, storage_backend: FlagStorageBackend) -> Self {
            self.0.set_storage_backend(storage_backend);
            self
        }

        fn build(self) -> Flag {
            self.0
        }
    }

    #[test]
    fn test_filter_container() {
        let flags = vec![
            TestFlagBuilder::new()
                .namespace("namespace")
                .name("test1")
                .package("package")
                .value(false)
                .permission(FlagPermission::FLAG_PERMISSION_READ_WRITE)
                .value_picked_from(ValuePickedFrom::VALUE_PICKED_FROM_DEFAULT)
                .container("system")
                .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD)
                .build(),
            TestFlagBuilder::new()
                .namespace("namespace")
                .name("test2")
                .package("package")
                .value(false)
                .permission(FlagPermission::FLAG_PERMISSION_READ_WRITE)
                .value_picked_from(ValuePickedFrom::VALUE_PICKED_FROM_DEFAULT)
                .container("not_system")
                .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD)
                .build(),
            TestFlagBuilder::new()
                .namespace("namespace")
                .name("test3")
                .package("package")
                .value(false)
                .permission(FlagPermission::FLAG_PERMISSION_READ_WRITE)
                .value_picked_from(ValuePickedFrom::VALUE_PICKED_FROM_DEFAULT)
                .container("system")
                .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD)
                .build(),
        ];

        assert_eq!((Filter { container: Some("system".to_string()) }).apply(&flags).len(), 2);
    }

    #[test]
    fn test_filter_no_container() {
        let flags = vec![
            TestFlagBuilder::new()
                .namespace("namespace")
                .name("test1")
                .package("package")
                .value(false)
                .permission(FlagPermission::FLAG_PERMISSION_READ_WRITE)
                .value_picked_from(ValuePickedFrom::VALUE_PICKED_FROM_DEFAULT)
                .container("system")
                .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD)
                .build(),
            TestFlagBuilder::new()
                .namespace("namespace")
                .name("test2")
                .package("package")
                .value(false)
                .permission(FlagPermission::FLAG_PERMISSION_READ_WRITE)
                .value_picked_from(ValuePickedFrom::VALUE_PICKED_FROM_DEFAULT)
                .container("not_system")
                .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD)
                .build(),
            TestFlagBuilder::new()
                .namespace("namespace")
                .name("test3")
                .package("package")
                .value(false)
                .permission(FlagPermission::FLAG_PERMISSION_READ_WRITE)
                .value_picked_from(ValuePickedFrom::VALUE_PICKED_FROM_DEFAULT)
                .container("system")
                .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD)
                .build(),
        ];

        assert_eq!((Filter { container: None }).apply(&flags).len(), 3);
    }

    #[test]
    #[cfg(not(feature = "cargo"))]
    fn test_set_unset_mainline_beta_flag() {
        let mut rng = rand::thread_rng();
        let namespace = rng.gen::<u32>().to_string();
        let mut flag = TestFlagBuilder::new()
            .namespace(&namespace)
            .name("some_flag")
            .package("some_package")
            .container("system")
            .value(false)
            .permission(FlagPermission::FLAG_PERMISSION_READ_WRITE)
            .value_picked_from(ValuePickedFrom::VALUE_PICKED_FROM_DEFAULT)
            .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD)
            .build();

        // negative test to ensure value is not synced over to device config
        assert!(set_flag(&flag, "false", false, &FLAG_SOURCES_PROVIDER).is_ok());

        let result = execute_device_config_command(&["list", &namespace]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key(&String::from("some_package.some_flag")));

        let result = execute_device_config_command(&["list", "device_config_overrides"]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key(&format!("{namespace}:some_package.some_flag")));

        // test setting mainline beta flag
        flag.set_storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_DEVICE_CONFIG);
        assert!(set_flag(&flag, "true", false, &FLAG_SOURCES_PROVIDER).is_ok());

        let result = execute_device_config_command(&["list", &namespace]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        let value = flags.get(&String::from("some_package.some_flag")).unwrap();
        assert_eq!(*value, "enabled".to_string());

        let result = execute_device_config_command(&["list", "device_config_overrides"]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        let value = flags.get(&format!("{namespace}:some_package.some_flag")).unwrap();
        assert_eq!(*value, "enabled".to_string());

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
            .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD)
            .build();
        let resolved = resolve_flag(Some(aconfigd_flag), None, None);
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.name(), "test_aconfigd_flag");
    }

    #[test]
    fn test_resolve_flag_aconfigd_with_mainline_beta_namespace() {
        let aconfigd_flag = TestFlagBuilder::new()
            .name("test_aconfigd_flag")
            .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD)
            .build();
        let mainline_beta_namespace =
            MainlineBetaNamespace { container: "test_container", allow_exported: false };
        let resolved = resolve_flag(Some(aconfigd_flag), None, Some(&mainline_beta_namespace));
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.name(), "test_aconfigd_flag");
    }

    #[test]
    fn test_resolve_flag_device_config_only_without_mainline_beta_namespace() {
        let device_config_flag = TestFlagBuilder::new()
            .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_DEVICE_CONFIG)
            .build();
        let resolved = resolve_flag(None, Some(device_config_flag), None);
        assert!(resolved.is_none());
    }

    #[test]
    fn test_resolve_flag_device_config_only_with_mainline_beta_namespace() {
        let device_config_flag = TestFlagBuilder::new()
            .name("test_device_config_flag")
            .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_DEVICE_CONFIG)
            .build();
        let mainline_beta_namespace =
            MainlineBetaNamespace { container: "test_container", allow_exported: false };
        let resolved = resolve_flag(None, Some(device_config_flag), Some(&mainline_beta_namespace));
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.name(), "test_device_config_flag");
    }

    #[test]
    fn test_resolve_flag_both_backends_without_mainline_beta_namespace() {
        let aconfigd_flag = TestFlagBuilder::new()
            .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD)
            .build();
        let device_config_flag = TestFlagBuilder::new()
            .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_DEVICE_CONFIG)
            .build();
        let resolved = resolve_flag(Some(aconfigd_flag), Some(device_config_flag), None);
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.storage_backend(), FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD);
    }

    #[test]
    fn test_resolve_flag_both_backends_with_mainline_beta_namespace_in_platform_container() {
        let aconfigd_flag = TestFlagBuilder::new()
            .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD)
            .container("platform_container")
            .build();
        let device_config_flag = TestFlagBuilder::new()
            .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_DEVICE_CONFIG)
            .build();
        let mainline_beta_namespace =
            MainlineBetaNamespace { container: "mainline_container", allow_exported: false };
        let resolved = resolve_flag(
            Some(aconfigd_flag),
            Some(device_config_flag),
            Some(&mainline_beta_namespace),
        );
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.storage_backend(), FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD);
    }

    #[test]
    fn test_resolve_flag_both_backends_with_mainline_beta_namespace_in_mainline_container() {
        let aconfigd_flag = TestFlagBuilder::new()
            .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD)
            .container("mainline_container")
            .build();
        let device_config_flag = TestFlagBuilder::new()
            .storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_DEVICE_CONFIG)
            .build();
        let mainline_beta_namespace =
            MainlineBetaNamespace { container: "mainline_container", allow_exported: false };
        let resolved = resolve_flag(
            Some(aconfigd_flag),
            Some(device_config_flag),
            Some(&mainline_beta_namespace),
        );
        assert!(resolved.is_some());
        let resolved = resolved.unwrap();
        assert_eq!(
            resolved.storage_backend(),
            FlagStorageBackend::FLAG_STORAGE_BACKEND_DEVICE_CONFIG
        );
    }

    #[test]
    fn test_proto_serialization() {
        let mut flag = Flag::new();
        flag.set_namespace("namespace".to_string());
        flag.set_name("test1".to_string());
        flag.set_package("package".to_string());
        flag.set_value(VALUE_DISABLED.to_string());
        flag.set_permission(FlagPermission::FLAG_PERMISSION_READ_WRITE);
        flag.set_value_picked_from(ValuePickedFrom::VALUE_PICKED_FROM_DEFAULT);
        flag.set_container("system".to_string());
        flag.set_storage_backend(FlagStorageBackend::FLAG_STORAGE_BACKEND_ACONFIGD);

        let mut flag_list = ProtoFlagList::new();
        flag_list.flags.push(flag.clone());

        let bytes = flag_list.write_to_bytes().unwrap();
        let parsed_list = ProtoFlagList::parse_from_bytes(&bytes).unwrap();

        assert_eq!(parsed_list.flags.len(), 1);
        assert_eq!(parsed_list.flags[0].name(), flag.name());
        assert_eq!(parsed_list.flags[0].package(), flag.package());
        assert_eq!(parsed_list.flags[0].value(), VALUE_DISABLED);
    }
}
