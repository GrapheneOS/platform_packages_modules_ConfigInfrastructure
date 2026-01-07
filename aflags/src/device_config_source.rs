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

use crate::{Flag, FlagPermission, FlagSource, FlagStorageBackend, FlagValue, ValuePickedFrom};

use anyhow::{anyhow, bail, Result};
use regex::Regex;
use std::collections::HashMap;
use std::process::Command;
use std::str;

pub struct DeviceConfigSource {}

pub(crate) fn parse_device_config_output(raw: &str) -> Result<HashMap<String, FlagValue>> {
    let mut flags = HashMap::new();
    let regex = Regex::new(r"(?m)^([[[:alnum:]]:_/.*]+)=(true|false)$")?;
    for capture in regex.captures_iter(raw) {
        let key =
            capture.get(1).ok_or(anyhow!("invalid device_config output"))?.as_str().to_string();
        let value = FlagValue::try_from(
            capture.get(2).ok_or(anyhow!("invalid device_config output"))?.as_str(),
        )?;
        flags.insert(key, value);
    }
    Ok(flags)
}

pub(crate) fn execute_device_config_command(command: &[&str]) -> Result<String> {
    let output = Command::new("/system/bin/device_config").args(command).output()?;
    if !output.status.success() {
        let reason = match output.status.code() {
            Some(code) => {
                let output = str::from_utf8(&output.stdout)?;
                if !output.is_empty() {
                    format!("exit code {code}, output was {output}")
                } else {
                    format!("exit code {code}")
                }
            }
            None => "terminated by signal".to_string(),
        };
        bail!("failed to access flag storage: {}", reason);
    }
    Ok(str::from_utf8(&output.stdout)?.to_string())
}

fn convert_staged_flag_name(staged_name: &str) -> Option<String> {
    match staged_name.find('*') {
        Some(star_index) => {
            let namespace = &staged_name[..star_index];
            let name = &staged_name[star_index + 1..];
            Some(format!("{namespace}/{name}"))
        }
        _ => None,
    }
}

fn extract_staged_flags(flags: HashMap<String, FlagValue>) -> HashMap<String, FlagValue> {
    let mut staged_flags = HashMap::new();

    for (staged_name, value) in flags {
        if let Some(name) = convert_staged_flag_name(&staged_name) {
            staged_flags.insert(name, value);
        }
    }

    staged_flags
}

fn read_device_config_flags() -> Result<HashMap<String, FlagValue>> {
    let output = execute_device_config_command(&["list"])?;
    parse_device_config_output(output.as_str())
}

fn read_staged_device_config_flags() -> Result<HashMap<String, FlagValue>> {
    let output = execute_device_config_command(&["list", "staged"])?;
    let staged_flag_map = parse_device_config_output(output.as_str())?;
    Ok(extract_staged_flags(staged_flag_map))
}

fn make_device_config_flag(
    name: &str,
    value: &FlagValue,
    staged_value: Option<&FlagValue>,
) -> Option<Flag> {
    let slash_index = name.find('/')?;
    let (namespace, name) = (&name[..slash_index], &name[slash_index + 1..]);
    let dot_index = name.rfind('.')?;
    let (package, name) = (&name[..dot_index], &name[dot_index + 1..]);

    Some(Flag {
        namespace: namespace.to_string(),
        name: name.to_string(),
        package: package.to_string(),
        container: "UNKNOWN_CONTAINER".to_string(), // TODO: Is there something better to put here?
        value: *value,
        staged_value: staged_value.cloned(),
        permission: FlagPermission::ReadWrite, // TODO: Is this correct?
        value_picked_from: ValuePickedFrom::Default, // TODO: Is this correct?
        storage_backend: FlagStorageBackend::DeviceConfig,
    })
}

impl FlagSource for DeviceConfigSource {
    fn list_flags(&self) -> Result<Vec<Flag>> {
        // Note: Since Mainline Beta flags are *not* listed in aconfig_flags.pb, we are not using
        // that file as a source of truth for flags. Therefore, these results are only useful for
        // merging with the flags from AconfigStorageSource, not displaying to the user directly.

        let flag_values = read_device_config_flags()?;
        let staged_flag_values = read_staged_device_config_flags()?;

        let flags = flag_values
            .iter()
            .filter_map(|(namespaced_name, value)| {
                let staged_value = staged_flag_values.get(namespaced_name);
                make_device_config_flag(namespaced_name, value, staged_value)
            })
            .collect();

        Ok(flags)
    }

    fn override_flag(
        &self,
        namespace: &str,
        qualified_name: &str,
        value: &str,
        _immediate: bool,
    ) -> Result<()> {
        // device config override command always immediate change the boot value
        execute_device_config_command(&["override", namespace, qualified_name, value]).map(|_| ())
    }

    fn unset_flag(&self, namespace: &str, qualified_name: &str, _immediate: bool) -> Result<()> {
        // device config clear_override command always clear the boot value as well
        execute_device_config_command(&["clear_override", namespace, qualified_name]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    const FLAG_SOURCE: DeviceConfigSource = DeviceConfigSource {};

    #[test]
    fn test_parse_device_config_output() {
        let input = r#"
namespace_one/com.foo.bar.flag_one=true
com.foo.bar.flag_two=false
random_noise;
namespace_two:android.flag_one=true
android.flag_two=nonsense
"#;
        let expected = HashMap::from([
            ("namespace_one/com.foo.bar.flag_one".to_string(), FlagValue::Enabled),
            ("com.foo.bar.flag_two".to_string(), FlagValue::Disabled),
            ("namespace_two:android.flag_one".to_string(), FlagValue::Enabled),
        ]);
        let actual = parse_device_config_output(input).unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn test_convert_staged_flag_name_valid() {
        assert_eq!(
            convert_staged_flag_name("namespace*package.name"),
            Some("namespace/package.name".to_string())
        );
    }

    #[test]
    fn test_convert_staged_flag_name_no_slash() {
        assert!(convert_staged_flag_name("namespace.package.name").is_none());
    }

    #[test]
    #[cfg(not(feature = "cargo"))]
    fn test_override_flag() {
        let mut rng = rand::thread_rng();
        let namespace = rng.gen::<u32>().to_string();
        FLAG_SOURCE
            .override_flag(&namespace, "aflags_test_package.aflags_test_flag", "false", false)
            .unwrap();

        let result = execute_device_config_command(&["list", &namespace]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        let flag_value = flags.get("aflags_test_package.aflags_test_flag").unwrap();
        assert_eq!(*flag_value, FlagValue::Disabled);

        let result = execute_device_config_command(&["list", "device_config_overrides"]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        println!("{flags:?}");
        let flag_value =
            flags.get(&format!("{namespace}:aflags_test_package.aflags_test_flag")).unwrap();
        assert_eq!(*flag_value, FlagValue::Disabled);

        FLAG_SOURCE.unset_flag(&namespace, "aflags_test_package.aflags_test_flag", false).unwrap();
    }

    #[test]
    #[cfg(not(feature = "cargo"))]
    fn test_unset_flag() {
        let mut rng = rand::thread_rng();
        let namespace = rng.gen::<u32>().to_string();
        FLAG_SOURCE
            .override_flag(&namespace, "aflags_test_package.aflags_test_flag", "false", false)
            .unwrap();
        FLAG_SOURCE.unset_flag(&namespace, "aflags_test_package.aflags_test_flag", false).unwrap();

        let result = execute_device_config_command(&["list", &namespace]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key("aflags_test_package.aflags_test_flag"));

        let result = execute_device_config_command(&["list", "device_config_overrides"]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key(&format!("{namespace}:aflags_test_package.aflags_test_flag")));
    }
}
