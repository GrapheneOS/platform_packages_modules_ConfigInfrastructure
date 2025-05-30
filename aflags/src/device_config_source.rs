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

use crate::{Flag, FlagSource, FlagValue};

use anyhow::{anyhow, bail, Result};
use regex::Regex;
use std::collections::HashMap;
use std::process::Command;
use std::str;

pub struct DeviceConfigSource {}

#[allow(dead_code)]
fn parse_device_config_output(raw: &str) -> Result<HashMap<String, FlagValue>> {
    let mut flags = HashMap::new();
    let regex = Regex::new(r"(?m)^([[[:alnum:]]:_/\.]+)=(true|false)$")?;
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

fn execute_device_config_command(command: &[&str]) -> Result<String> {
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

impl FlagSource for DeviceConfigSource {
    fn list_flags() -> Result<Vec<Flag>> {
        Err(anyhow!("new storage should be source of truth"))
    }

    fn override_flag(
        namespace: &str,
        qualified_name: &str,
        value: &str,
        _immediate: bool,
    ) -> Result<()> {
        // device config override command always immediate change the boot value
        execute_device_config_command(&["override", namespace, qualified_name, value]).map(|_| ())
    }

    fn unset_flag(namespace: &str, qualified_name: &str, _immediate: bool) -> Result<()> {
        // device config clear_override command always clear the boot value as well
        execute_device_config_command(&["clear_override", namespace, qualified_name]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

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
    #[cfg(not(feature = "cargo"))]
    fn test_override_flag() {
        let mut rng = rand::thread_rng();
        let namespace = rng.gen::<u32>().to_string();
        DeviceConfigSource::override_flag(
            &namespace,
            "aflags_test_package.aflags_test_flag",
            "false",
            false,
        )
        .unwrap();

        let result = execute_device_config_command(&["list", &namespace]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        let flag_value = flags.get("aflags_test_package.aflags_test_flag").unwrap();
        assert_eq!(*flag_value, FlagValue::Disabled);

        let result = execute_device_config_command(&["list", "device_config_overrides"]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        println!("{:?}", flags);
        let flag_value =
            flags.get(&format!("{namespace}:aflags_test_package.aflags_test_flag")).unwrap();
        assert_eq!(*flag_value, FlagValue::Disabled);
    }

    #[test]
    #[cfg(not(feature = "cargo"))]
    fn test_unset_flag() {
        let mut rng = rand::thread_rng();
        let namespace = rng.gen::<u32>().to_string();
        DeviceConfigSource::override_flag(
            &namespace,
            "aflags_test_package.aflags_test_flag",
            "false",
            false,
        )
        .unwrap();
        DeviceConfigSource::unset_flag(&namespace, "aflags_test_package.aflags_test_flag", false)
            .unwrap();

        let result = execute_device_config_command(&["list", &namespace]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key("aflags_test_package.aflags_test_flag"));

        let result = execute_device_config_command(&["list", "device_config_overrides"]).unwrap();
        let flags = parse_device_config_output(&result).unwrap();
        assert!(!flags.contains_key(&format!("{namespace}:aflags_test_package.aflags_test_flag")));
    }
}
