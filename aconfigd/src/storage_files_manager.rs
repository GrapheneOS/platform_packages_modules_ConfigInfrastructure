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

use crate::storage_files::{FlagSnapshot, StorageFiles};
use crate::utils::{get_files_digest, read_pb_from_file, remove_file, write_pb_to_file};
use crate::AconfigdError;
use aconfigd_protos::{
    ProtoFlagOverride, ProtoFlagOverrideType, ProtoLocalFlagOverrides, ProtoOTAFlagStagingMessage,
    ProtoPersistStorageRecord, ProtoPersistStorageRecords, ProtoRemoveOverrideType,
};
use log::debug;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// Storage files manager to manage all the storage files across containers
#[derive(Debug)]
pub(crate) struct StorageFilesManager {
    pub root_dir: PathBuf,
    pub all_storage_files: HashMap<String, StorageFiles>,
    pub package_to_container: HashMap<String, String>,
}

impl StorageFilesManager {
    /// Constructor
    pub(crate) fn new(root_dir: &Path) -> Self {
        Self {
            root_dir: root_dir.to_path_buf(),
            all_storage_files: HashMap::new(),
            package_to_container: HashMap::new(),
        }
    }

    /// Get storage files for a container
    fn get_storage_files(&mut self, container: &str) -> Option<&mut StorageFiles> {
        self.all_storage_files.get_mut(container)
    }

    /// Add storage files based on a storage record pb entry
    pub(crate) fn add_storage_files_from_pb(
        &mut self,
        pb: &ProtoPersistStorageRecord,
    ) -> Result<(), AconfigdError> {
        if self.all_storage_files.contains_key(pb.container()) {
            debug!(
                "Ignored request to add storage files from pb for {}, already exists",
                pb.container()
            );
            return Ok(());
        }

        if aconfig_new_storage_flags::bluetooth_flag_value_bug_fix() {
            // Only create storage file object if the container is active. This is to
            // ensure that for inactive containers, the storage files object will not
            // be created and thus their boot copy will not be produced. And thus we
            // can prevent them from being used.
            if PathBuf::from(pb.package_map()).exists()
                && PathBuf::from(pb.flag_map()).exists()
                && PathBuf::from(pb.flag_val()).exists()
                && PathBuf::from(pb.flag_info()).exists()
            {
                self.all_storage_files.insert(
                    String::from(pb.container()),
                    StorageFiles::from_pb(pb, &self.root_dir)?,
                );
            }
        } else {
            self.all_storage_files
                .insert(String::from(pb.container()), StorageFiles::from_pb(pb, &self.root_dir)?);
        }

        Ok(())
    }

    /// Add a new container's storage files
    fn add_storage_files_from_container(
        &mut self,
        container: &str,
        default_package_map: &Path,
        default_flag_map: &Path,
        default_flag_val: &Path,
        default_flag_info: &Path,
    ) -> Result<&mut StorageFiles, AconfigdError> {
        if self.all_storage_files.contains_key(container) {
            debug!(
                "Ignored request to add storage files from container {}, already exists",
                container
            );
        }

        self.all_storage_files.insert(
            String::from(container),
            StorageFiles::from_container(
                container,
                default_package_map,
                default_flag_map,
                default_flag_val,
                default_flag_info,
                &self.root_dir,
            )?,
        );

        self.all_storage_files
            .get_mut(container)
            .ok_or(AconfigdError::FailToGetStorageFiles { container: container.to_string() })
    }

    /// Update a container's storage files in the case of container update
    fn update_container_storage_files(
        &mut self,
        container: &str,
        default_package_map: &Path,
        default_flag_map: &Path,
        default_flag_val: &Path,
        default_flag_info: &Path,
    ) -> Result<(), AconfigdError> {
        debug!("update {} storage files", container);
        let mut storage_files = self
            .get_storage_files(container)
            .ok_or(AconfigdError::FailToGetStorageFiles { container: container.to_string() })?;

        // backup overrides
        let server_overrides = storage_files.get_all_server_overrides()?;
        let local_overrides = storage_files.get_all_local_overrides()?;

        // recreate storage files object
        storage_files.remove_persist_files()?;
        self.all_storage_files.remove(container);
        storage_files = self.add_storage_files_from_container(
            container,
            default_package_map,
            default_flag_map,
            default_flag_val,
            default_flag_info,
        )?;

        // restage server overrides
        debug!("restaging existing server overrides");
        for f in server_overrides.iter() {
            let context = storage_files.get_package_flag_context(&f.package_name, &f.flag_name)?;
            if context.flag_exists {
                storage_files.stage_server_override(&context, &f.flag_value)?;
            }
        }

        // restage local overrides
        debug!("restaging existing local overrides");
        let mut new_pb = ProtoLocalFlagOverrides::new();
        for f in local_overrides.into_iter() {
            let context =
                storage_files.get_package_flag_context(f.package_name(), f.flag_name())?;
            if context.flag_exists {
                storage_files.stage_local_override(&context, f.flag_value())?;
                new_pb.overrides.push(f);
            }
        }
        write_pb_to_file::<ProtoLocalFlagOverrides>(
            &new_pb,
            &storage_files.storage_record.local_overrides,
        )?;

        Ok(())
    }

    /// add or update a container's storage files in the case of container
    /// update
    pub(crate) fn add_or_update_container_storage_files(
        &mut self,
        container: &str,
        default_package_map: &Path,
        default_flag_map: &Path,
        default_flag_val: &Path,
        default_flag_info: &Path,
    ) -> Result<(), AconfigdError> {
        match self.get_storage_files(container) {
            Some(storage_files) => {
                let digest = get_files_digest(
                    &[default_package_map, default_flag_map, default_flag_val, default_flag_info][..],
                )?;
                if storage_files.storage_record.digest != digest {
                    self.update_container_storage_files(
                        container,
                        default_package_map,
                        default_flag_map,
                        default_flag_val,
                        default_flag_info,
                    )?;
                } else {
                    debug!("no need to update {}, computed digest matches with record", container);
                }
            }
            None => {
                self.add_storage_files_from_container(
                    container,
                    default_package_map,
                    default_flag_map,
                    default_flag_val,
                    default_flag_info,
                )?;
            }
        }

        Ok(())
    }

    /// Apply all staged server and local overrides
    pub(crate) fn apply_all_staged_overrides(
        &mut self,
        container: &str,
    ) -> Result<(), AconfigdError> {
        let storage_files = self
            .get_storage_files(container)
            .ok_or(AconfigdError::FailToGetStorageFiles { container: container.to_string() })?;
        storage_files.apply_all_staged_overrides()?;
        Ok(())
    }

    /// Reset all storage files
    pub(crate) fn reset_all_storage(&mut self) -> Result<(), AconfigdError> {
        debug!("reset storage files of all containers");
        let all_containers = self.all_storage_files.keys().cloned().collect::<Vec<String>>();
        for container in all_containers {
            let storage_files = self
                .get_storage_files(&container)
                .ok_or(AconfigdError::FailToGetStorageFiles { container: container.to_string() })?;

            let record = storage_files.storage_record.clone();
            storage_files.remove_persist_files()?;
            self.all_storage_files.remove(&container);

            self.add_storage_files_from_container(
                &container,
                &record.default_package_map,
                &record.default_flag_map,
                &record.default_flag_val,
                &record.default_flag_info,
            )?;
        }
        Ok(())
    }

    /// Get container
    fn get_container(&mut self, package: &str) -> Result<Option<String>, AconfigdError> {
        match self.package_to_container.get(package) {
            Some(container) => Ok(Some(container.clone())),
            None => {
                for (container, storage_files) in &mut self.all_storage_files {
                    if storage_files.has_package(package)? {
                        self.package_to_container.insert(String::from(package), container.clone());
                        return Ok(Some(container.clone()));
                    }
                }
                Ok(None)
            }
        }
    }

    /// Apply flag override
    pub(crate) fn override_flag_value(
        &mut self,
        package: &str,
        flag: &str,
        value: &str,
        override_type: ProtoFlagOverrideType,
    ) -> Result<(), AconfigdError> {
        let container = self
            .get_container(package)?
            .ok_or(AconfigdError::FailToFindContainer { package: package.to_string() })?;

        let storage_files = self
            .get_storage_files(&container)
            .ok_or(AconfigdError::FailToGetStorageFiles { container: container.to_string() })?;

        let context = storage_files.get_package_flag_context(package, flag)?;
        match override_type {
            ProtoFlagOverrideType::SERVER_ON_REBOOT => {
                storage_files.stage_server_override(&context, value)?;
            }
            ProtoFlagOverrideType::LOCAL_ON_REBOOT => {
                storage_files.stage_local_override(&context, value)?;
            }
            ProtoFlagOverrideType::LOCAL_IMMEDIATE => {
                storage_files.stage_and_apply_local_override(&context, value)?;
            }
        }

        Ok(())
    }

    /// Read staged ota flags
    fn get_ota_flags(&mut self) -> Result<Option<Vec<ProtoFlagOverride>>, AconfigdError> {
        let ota_pb_file = self.root_dir.join("flags/ota.pb");
        if !ota_pb_file.exists() {
            debug!("no OTA flags staged, skip");
            return Ok(None);
        }

        let ota_flags_pb = read_pb_from_file::<ProtoOTAFlagStagingMessage>(&ota_pb_file)?;
        if let Some(target_build_id) = ota_flags_pb.build_id {
            let device_build_id = rustutils::system_properties::read("ro.build.fingerprint")
                .map_err(|errmsg| AconfigdError::FailToReadBuildFingerPrint { errmsg })?;
            if device_build_id == Some(target_build_id.clone()) {
                remove_file(&ota_pb_file)?;
                Ok(Some(ota_flags_pb.overrides))
            } else {
                debug!(
                    "fingerprint mismatch between OTA flag staging {}, and device {}",
                    target_build_id,
                    device_build_id.unwrap_or(String::from("None")),
                );
                Ok(None)
            }
        } else {
            debug!("ill formatted OTA staged flags, build fingerprint not set");
            remove_file(&ota_pb_file)?;
            return Ok(None);
        }
    }

    /// Apply staged ota flags
    pub(crate) fn apply_staged_ota_flags(&mut self) -> Result<(), AconfigdError> {
        if let Some(flags) = self.get_ota_flags()? {
            debug!("apply staged OTA flags");
            for flag in flags.iter() {
                if let Err(errmsg) = self.override_flag_value(
                    flag.package_name(),
                    flag.flag_name(),
                    flag.flag_value(),
                    ProtoFlagOverrideType::SERVER_ON_REBOOT,
                ) {
                    debug!(
                        "failed to apply ota flag override for {}.{}: {:?}",
                        flag.package_name(),
                        flag.flag_name(),
                        errmsg
                    );
                }
            }
        }
        Ok(())
    }

    /// Write persist storage records to file
    pub(crate) fn write_persist_storage_records_to_file(
        &self,
        file: &Path,
    ) -> Result<(), AconfigdError> {
        debug!("writing updated storage records {}", file.display().to_string());
        let mut pb = ProtoPersistStorageRecords::new();
        pb.records = self
            .all_storage_files
            .values()
            .map(|storage_files| {
                let record = &storage_files.storage_record;
                let mut entry = ProtoPersistStorageRecord::new();
                entry.set_version(record.version);
                entry.set_container(record.container.clone());
                entry.set_package_map(record.default_package_map.display().to_string());
                entry.set_flag_map(record.default_flag_map.display().to_string());
                entry.set_flag_val(record.default_flag_val.display().to_string());
                entry.set_flag_info(record.default_flag_info.display().to_string());
                entry.set_digest(record.digest.clone());
                entry
            })
            .collect();
        write_pb_to_file(&pb, file)
    }

    /// Remove a single local override
    pub(crate) fn remove_local_override(
        &mut self,
        package: &str,
        flag: &str,
        remove_override_type: ProtoRemoveOverrideType,
    ) -> Result<(), AconfigdError> {
        let container = self
            .get_container(package)?
            .ok_or(AconfigdError::FailToFindContainer { package: package.to_string() })?;

        let storage_files = self
            .get_storage_files(&container)
            .ok_or(AconfigdError::FailToGetStorageFiles { container: container.to_string() })?;

        let context = storage_files.get_package_flag_context(package, flag)?;
        let immediate = remove_override_type == ProtoRemoveOverrideType::REMOVE_LOCAL_IMMEDIATE;
        storage_files.remove_local_override(&context, immediate)
    }

    /// Remove all local overrides
    pub(crate) fn remove_all_local_overrides(&mut self) -> Result<(), AconfigdError> {
        debug!("remove all local overrides for all containers");
        for storage_files in self.all_storage_files.values_mut() {
            storage_files.remove_all_local_overrides()?;
        }
        Ok(())
    }

    /// Get flag snapshot
    pub(crate) fn get_flag_snapshot(
        &mut self,
        package: &str,
        flag: &str,
    ) -> Result<Option<FlagSnapshot>, AconfigdError> {
        match self.get_container(package)? {
            Some(container) => {
                let storage_files = self.get_storage_files(&container).ok_or(
                    AconfigdError::FailToGetStorageFiles { container: container.to_string() },
                )?;

                storage_files.get_flag_snapshot(package, flag)
            }
            None => Ok(None),
        }
    }

    /// List all flags in a package
    pub(crate) fn list_flags_in_package(
        &mut self,
        package: &str,
    ) -> Result<Vec<FlagSnapshot>, AconfigdError> {
        let container = self
            .get_container(package)?
            .ok_or(AconfigdError::FailToFindContainer { package: package.to_string() })?;

        let storage_files = self
            .get_storage_files(&container)
            .ok_or(AconfigdError::FailToGetStorageFiles { container: container.to_string() })?;

        storage_files.list_flags_in_package(package)
    }

    /// List flags in a container
    pub(crate) fn list_flags_in_container(
        &mut self,
        container: &str,
    ) -> Result<Vec<FlagSnapshot>, AconfigdError> {
        let storage_files = self
            .get_storage_files(&container)
            .ok_or(AconfigdError::FailToGetStorageFiles { container: container.to_string() })?;

        storage_files.list_all_flags()
    }

    /// List all the flags
    pub(crate) fn list_all_flags(&mut self) -> Result<Vec<FlagSnapshot>, AconfigdError> {
        debug!("list all flags across containers");
        let mut flags = Vec::new();
        for storage_files in self.all_storage_files.values_mut() {
            match storage_files.list_all_flags() {
                Ok(f) => {
                    flags.extend(f);
                }
                Err(errmsg) => {
                    debug!(
                        "failed to list all flags for {}: {:?}",
                        storage_files.storage_record.container, errmsg
                    );
                }
            }
        }
        Ok(flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage_files::StorageRecord;
    use crate::test_utils::{has_same_content, ContainerMock, StorageRootDirMock};
    use crate::utils::{copy_file, get_files_digest, read_pb_from_file};
    use aconfig_storage_file::{FlagValueSummary, StoredFlagType};
    use aconfigd_protos::ProtoFlagOverride;

    #[test]
    fn test_add_storage_files_from_pb() {
        let root_dir = StorageRootDirMock::new();
        let container = ContainerMock::new();

        let persist_package_map = root_dir.maps_dir.join("mockup.package.map");
        let persist_flag_map = root_dir.maps_dir.join("mockup.flag.map");
        let persist_flag_val = root_dir.flags_dir.join("mockup.val");
        let persist_flag_info = root_dir.flags_dir.join("mockup.info");
        copy_file(&container.package_map, &persist_package_map, 0o444).unwrap();
        copy_file(&container.flag_map, &persist_flag_map, 0o444).unwrap();
        copy_file(&container.flag_val, &persist_flag_val, 0o644).unwrap();
        copy_file(&container.flag_info, &persist_flag_info, 0o644).unwrap();

        let mut pb = ProtoPersistStorageRecord::new();
        pb.set_version(123);
        pb.set_container("mockup".to_string());
        pb.set_package_map(container.package_map.display().to_string());
        pb.set_flag_map(container.flag_map.display().to_string());
        pb.set_flag_val(container.flag_val.display().to_string());
        pb.set_flag_info(container.flag_info.display().to_string());
        pb.set_digest(String::from("abc"));

        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        manager.add_storage_files_from_pb(&pb);
        assert_eq!(manager.all_storage_files.len(), 1);
        assert_eq!(
            manager.all_storage_files.get("mockup").unwrap(),
            &StorageFiles::from_pb(&pb, &root_dir.tmp_dir.path()).unwrap(),
        );

        // Ensure we can run this again, to exercise the case where the storage
        // files already exist, for example if the storage proto is deleted.
        manager.add_storage_files_from_pb(&pb);
        assert_eq!(manager.all_storage_files.len(), 1);
        assert_eq!(
            manager.all_storage_files.get("mockup").unwrap(),
            &StorageFiles::from_pb(&pb, &root_dir.tmp_dir.path()).unwrap(),
        );
    }

    fn init_storage(container: &ContainerMock, manager: &mut StorageFilesManager) {
        manager
            .add_or_update_container_storage_files(
                &container.name,
                &container.package_map,
                &container.flag_map,
                &container.flag_val,
                &container.flag_info,
            )
            .unwrap();
    }

    #[test]
    fn test_add_storage_files_from_container() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);

        let storage_files = manager.get_storage_files(&container.name).unwrap();

        let expected_record = StorageRecord {
            version: 1,
            container: String::from("mockup"),
            default_package_map: container.package_map.clone(),
            default_flag_map: container.flag_map.clone(),
            default_flag_val: container.flag_val.clone(),
            default_flag_info: container.flag_info.clone(),
            persist_package_map: root_dir.maps_dir.join("mockup.package.map"),
            persist_flag_map: root_dir.maps_dir.join("mockup.flag.map"),
            persist_flag_val: root_dir.flags_dir.join("mockup.val"),
            persist_flag_info: root_dir.flags_dir.join("mockup.info"),
            local_overrides: root_dir.flags_dir.join("mockup_local_overrides.pb"),
            boot_flag_val: root_dir.boot_dir.join("mockup.val"),
            boot_flag_info: root_dir.boot_dir.join("mockup.info"),
            digest: get_files_digest(
                &[
                    container.package_map.as_path(),
                    container.flag_map.as_path(),
                    container.flag_val.as_path(),
                    container.flag_info.as_path(),
                ][..],
            )
            .unwrap(),
        };

        let expected_storage_files = StorageFiles {
            storage_record: expected_record,
            package_map: None,
            flag_map: None,
            flag_val: None,
            boot_flag_val: None,
            boot_flag_info: None,
            persist_flag_val: None,
            persist_flag_info: None,
            mutable_boot_flag_val: None,
            mutable_boot_flag_info: None,
        };

        assert_eq!(storage_files, &expected_storage_files);

        assert!(has_same_content(
            &container.package_map,
            &storage_files.storage_record.persist_package_map
        ));
        assert!(has_same_content(
            &container.flag_map,
            &storage_files.storage_record.persist_flag_map
        ));
        assert!(has_same_content(
            &container.flag_val,
            &storage_files.storage_record.persist_flag_val
        ));
        assert!(has_same_content(
            &container.flag_info,
            &storage_files.storage_record.persist_flag_info
        ));
        assert!(has_same_content(&container.flag_val, &storage_files.storage_record.boot_flag_val));
        assert!(has_same_content(
            &container.flag_info,
            &storage_files.storage_record.boot_flag_info
        ));
    }

    #[test]
    fn test_simple_update_container_storage_files() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);

        // copy files over to mimic a container update
        std::fs::copy("./tests/data/container_with_more_flags.package.map", &container.package_map)
            .unwrap();
        std::fs::copy("./tests/data/container_with_more_flags.flag.map", &container.flag_map)
            .unwrap();
        std::fs::copy("./tests/data/container_with_more_flags.flag.val", &container.flag_val)
            .unwrap();
        std::fs::copy("./tests/data/container_with_more_flags.flag.info", &container.flag_info)
            .unwrap();

        // update container
        manager
            .add_or_update_container_storage_files(
                &container.name,
                &container.package_map,
                &container.flag_map,
                &container.flag_val,
                &container.flag_info,
            )
            .unwrap();

        let storage_files = manager.get_storage_files(&container.name).unwrap();

        assert!(has_same_content(
            &Path::new("./tests/data/container_with_more_flags.package.map"),
            &storage_files.storage_record.persist_package_map
        ));
        assert!(has_same_content(
            &Path::new("./tests/data/container_with_more_flags.flag.map"),
            &storage_files.storage_record.persist_flag_map
        ));
        assert!(has_same_content(
            &Path::new("./tests/data/container_with_more_flags.flag.val"),
            &storage_files.storage_record.persist_flag_val
        ));
        assert!(has_same_content(
            &Path::new("./tests/data/container_with_more_flags.flag.info"),
            &storage_files.storage_record.persist_flag_info
        ));
        assert!(has_same_content(
            &Path::new("./tests/data/container_with_more_flags.flag.val"),
            &storage_files.storage_record.boot_flag_val
        ));
        assert!(has_same_content(
            &Path::new("./tests/data/container_with_more_flags.flag.info"),
            &storage_files.storage_record.boot_flag_info
        ));
        assert!(storage_files.storage_record.local_overrides.exists());
    }

    fn add_example_overrides(manager: &mut StorageFilesManager) {
        manager
            .override_flag_value(
                "com.android.aconfig.storage.test_1",
                "enabled_rw",
                "false",
                ProtoFlagOverrideType::SERVER_ON_REBOOT,
            )
            .unwrap();

        manager
            .override_flag_value(
                "com.android.aconfig.storage.test_1",
                "disabled_rw",
                "false",
                ProtoFlagOverrideType::SERVER_ON_REBOOT,
            )
            .unwrap();

        manager
            .override_flag_value(
                "com.android.aconfig.storage.test_1",
                "disabled_rw",
                "true",
                ProtoFlagOverrideType::LOCAL_ON_REBOOT,
            )
            .unwrap();
    }

    #[test]
    fn test_overrides_after_update_container_storage_files() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);
        add_example_overrides(&mut manager);

        // copy files over to mimic a container update
        std::fs::copy("./tests/data/package.map", &container.package_map).unwrap();
        std::fs::copy("./tests/data/flag.map", &container.flag_map).unwrap();
        std::fs::copy("./tests/data/flag.val", &container.flag_val).unwrap();
        std::fs::copy("./tests/data/flag.info", &container.flag_info).unwrap();

        // update container
        manager
            .add_or_update_container_storage_files(
                &container.name,
                &container.package_map,
                &container.flag_map,
                &container.flag_val,
                &container.flag_info,
            )
            .unwrap();

        // verify that server override is persisted
        let storage_files = manager.get_storage_files(&container.name).unwrap();
        let server_overrides = storage_files.get_all_server_overrides().unwrap();
        assert_eq!(server_overrides.len(), 2);
        assert_eq!(
            server_overrides[0],
            FlagValueSummary {
                package_name: "com.android.aconfig.storage.test_1".to_string(),
                flag_name: "disabled_rw".to_string(),
                flag_value: "false".to_string(),
                value_type: StoredFlagType::ReadWriteBoolean,
            }
        );
        assert_eq!(
            server_overrides[1],
            FlagValueSummary {
                package_name: "com.android.aconfig.storage.test_1".to_string(),
                flag_name: "enabled_rw".to_string(),
                flag_value: "false".to_string(),
                value_type: StoredFlagType::ReadWriteBoolean,
            }
        );

        // verify that local override is persisted
        let local_overrides = storage_files.get_all_local_overrides().unwrap();
        assert_eq!(local_overrides.len(), 1);
        let mut pb = ProtoFlagOverride::new();
        pb.set_package_name("com.android.aconfig.storage.test_1".to_string());
        pb.set_flag_name("disabled_rw".to_string());
        pb.set_flag_value("true".to_string());
        assert_eq!(local_overrides[0], pb);
    }

    #[test]
    fn test_apply_all_staged_overrides() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);
        add_example_overrides(&mut manager);
        manager.apply_all_staged_overrides("mockup").unwrap();

        let mut flag =
            manager.get_flag_snapshot("com.android.aconfig.storage.test_1", "enabled_rw").unwrap();

        let mut expected_flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_1"),
            flag: String::from("enabled_rw"),
            server_value: String::from("false"),
            local_value: String::new(),
            boot_value: String::from("false"),
            default_value: String::from("true"),
            is_readwrite: true,
            has_server_override: true,
            has_local_override: false,
            has_boot_local_override: false,
        };

        assert_eq!(flag, Some(expected_flag));

        flag =
            manager.get_flag_snapshot("com.android.aconfig.storage.test_1", "disabled_rw").unwrap();

        expected_flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_1"),
            flag: String::from("disabled_rw"),
            server_value: String::from("false"),
            local_value: String::from("true"),
            boot_value: String::from("true"),
            default_value: String::from("false"),
            is_readwrite: true,
            has_server_override: true,
            has_local_override: true,
            has_boot_local_override: false,
        };

        assert_eq!(flag, Some(expected_flag));
    }

    #[test]
    fn test_reset_all_storage() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);
        add_example_overrides(&mut manager);
        manager.apply_all_staged_overrides("mockup").unwrap();

        manager.reset_all_storage().unwrap();
        let storage_files = manager.get_storage_files(&container.name).unwrap();
        assert!(has_same_content(
            &container.flag_val,
            &storage_files.storage_record.persist_flag_val
        ));
        assert!(has_same_content(
            &container.flag_info,
            &storage_files.storage_record.persist_flag_info
        ));
        assert!(has_same_content(&container.flag_val, &storage_files.storage_record.boot_flag_val));
        assert!(has_same_content(
            &container.flag_info,
            &storage_files.storage_record.boot_flag_info
        ));
    }

    fn test_override_flag_server_on_reboot() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);
        manager.apply_all_staged_overrides("mockup").unwrap();

        manager
            .override_flag_value(
                "com.android.aconfig.storage.test_1",
                "enabled_rw",
                "false",
                ProtoFlagOverrideType::SERVER_ON_REBOOT,
            )
            .unwrap();

        let flag =
            manager.get_flag_snapshot("com.android.aconfig.storage.test_1", "enabled_rw").unwrap();

        let expected_flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_1"),
            flag: String::from("enabled_rw"),
            server_value: String::from("false"),
            local_value: String::new(),
            boot_value: String::from("true"),
            default_value: String::from("true"),
            is_readwrite: true,
            has_server_override: true,
            has_local_override: false,
            has_boot_local_override: false,
        };

        assert_eq!(flag, Some(expected_flag));
    }

    #[test]
    fn test_override_flag_local_on_reboot() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);
        manager.apply_all_staged_overrides("mockup").unwrap();

        manager
            .override_flag_value(
                "com.android.aconfig.storage.test_1",
                "enabled_rw",
                "false",
                ProtoFlagOverrideType::LOCAL_ON_REBOOT,
            )
            .unwrap();

        let flag =
            manager.get_flag_snapshot("com.android.aconfig.storage.test_1", "enabled_rw").unwrap();

        let expected_flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_1"),
            flag: String::from("enabled_rw"),
            server_value: String::new(),
            local_value: String::from("false"),
            boot_value: String::from("true"),
            default_value: String::from("true"),
            is_readwrite: true,
            has_server_override: false,
            has_local_override: true,
            has_boot_local_override: false,
        };

        assert_eq!(flag, Some(expected_flag));
    }

    #[test]
    fn test_override_flag_local_immediate() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);
        manager.apply_all_staged_overrides("mockup").unwrap();

        manager
            .override_flag_value(
                "com.android.aconfig.storage.test_1",
                "enabled_rw",
                "false",
                ProtoFlagOverrideType::LOCAL_IMMEDIATE,
            )
            .unwrap();

        let flag =
            manager.get_flag_snapshot("com.android.aconfig.storage.test_1", "enabled_rw").unwrap();

        let expected_flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_1"),
            flag: String::from("enabled_rw"),
            server_value: String::new(),
            local_value: String::from("false"),
            boot_value: String::from("false"),
            default_value: String::from("true"),
            is_readwrite: true,
            has_server_override: false,
            has_local_override: true,
            has_boot_local_override: false,
        };

        assert_eq!(flag, Some(expected_flag));
    }

    #[test]
    fn test_get_ota_flags() {
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());

        let mut ota_flags = ProtoOTAFlagStagingMessage::new();
        ota_flags.set_build_id("xyz.123".to_string());
        write_pb_to_file::<ProtoOTAFlagStagingMessage>(
            &ota_flags,
            &root_dir.flags_dir.join("ota.pb"),
        )
        .unwrap();
        let staged_ota_flags = manager.get_ota_flags().unwrap();
        assert!(staged_ota_flags.is_none());
        assert!(root_dir.flags_dir.join("ota.pb").exists());

        let device_build_id =
            rustutils::system_properties::read("ro.build.fingerprint").unwrap().unwrap();
        ota_flags.set_build_id(device_build_id);
        let mut flag1 = ProtoFlagOverride::new();
        flag1.set_package_name("com.android.aconfig.storage.test_1".to_string());
        flag1.set_flag_name("enabled_rw".to_string());
        flag1.set_flag_value("false".to_string());
        ota_flags.overrides.push(flag1.clone());
        let mut flag2 = ProtoFlagOverride::new();
        flag2.set_package_name("com.android.aconfig.storage.test_2".to_string());
        flag2.set_flag_name("disabled_rw".to_string());
        flag2.set_flag_value("true".to_string());
        ota_flags.overrides.push(flag2.clone());
        write_pb_to_file::<ProtoOTAFlagStagingMessage>(
            &ota_flags,
            &root_dir.flags_dir.join("ota.pb"),
        )
        .unwrap();
        let staged_ota_flags = manager.get_ota_flags().unwrap().unwrap();
        assert_eq!(staged_ota_flags.len(), 2);
        assert_eq!(staged_ota_flags[0], flag1);
        assert_eq!(staged_ota_flags[1], flag2);
        assert!(!root_dir.flags_dir.join("ota.pb").exists());
    }

    #[test]
    fn test_apply_staged_ota_flags() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);

        let mut ota_flags = ProtoOTAFlagStagingMessage::new();
        let device_build_id =
            rustutils::system_properties::read("ro.build.fingerprint").unwrap().unwrap();
        ota_flags.set_build_id(device_build_id);
        let mut flag1 = ProtoFlagOverride::new();
        flag1.set_package_name("com.android.aconfig.storage.test_1".to_string());
        flag1.set_flag_name("enabled_rw".to_string());
        flag1.set_flag_value("false".to_string());
        ota_flags.overrides.push(flag1.clone());
        let mut flag2 = ProtoFlagOverride::new();
        flag2.set_package_name("com.android.aconfig.storage.test_2".to_string());
        flag2.set_flag_name("disabled_rw".to_string());
        flag2.set_flag_value("true".to_string());
        ota_flags.overrides.push(flag2.clone());
        let mut flag3 = ProtoFlagOverride::new();
        flag3.set_package_name("not_exist".to_string());
        flag3.set_flag_name("not_exist".to_string());
        flag3.set_flag_value("true".to_string());
        ota_flags.overrides.push(flag3.clone());
        write_pb_to_file::<ProtoOTAFlagStagingMessage>(
            &ota_flags,
            &root_dir.flags_dir.join("ota.pb"),
        )
        .unwrap();

        manager.apply_staged_ota_flags().unwrap();
        let storage_files = manager.get_storage_files(&container.name).unwrap();
        let server_overrides = storage_files.get_all_server_overrides().unwrap();
        assert_eq!(server_overrides.len(), 2);
        assert_eq!(
            server_overrides[0].package_name,
            "com.android.aconfig.storage.test_1".to_string()
        );
        assert_eq!(server_overrides[0].flag_name, "enabled_rw".to_string());
        assert_eq!(server_overrides[0].flag_value, "false".to_string());
        assert_eq!(
            server_overrides[1].package_name,
            "com.android.aconfig.storage.test_2".to_string()
        );
        assert_eq!(server_overrides[1].flag_name, "disabled_rw".to_string());
        assert_eq!(server_overrides[1].flag_value, "true".to_string());
    }

    #[test]
    fn test_write_persist_storage_records_to_file() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);

        let pb_file = root_dir.tmp_dir.path().join("records.pb");
        manager.write_persist_storage_records_to_file(&pb_file).unwrap();

        let pb = read_pb_from_file::<ProtoPersistStorageRecords>(&pb_file).unwrap();
        assert_eq!(pb.records.len(), 1);

        let mut entry = ProtoPersistStorageRecord::new();
        entry.set_version(1);
        entry.set_container("mockup".to_string());
        entry.set_package_map(container.package_map.display().to_string());
        entry.set_flag_map(container.flag_map.display().to_string());
        entry.set_flag_val(container.flag_val.display().to_string());
        entry.set_flag_info(container.flag_info.display().to_string());
        let digest = get_files_digest(
            &[
                container.package_map.as_path(),
                container.flag_map.as_path(),
                container.flag_val.as_path(),
                container.flag_info.as_path(),
            ][..],
        )
        .unwrap();
        entry.set_digest(digest);
        assert_eq!(pb.records[0], entry);
    }

    #[test]
    fn test_remove_local_override() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);
        add_example_overrides(&mut manager);
        manager.apply_all_staged_overrides("mockup").unwrap();

        manager
            .remove_local_override(
                "com.android.aconfig.storage.test_1",
                "disabled_rw",
                ProtoRemoveOverrideType::REMOVE_LOCAL_ON_REBOOT,
            )
            .unwrap();

        let flag =
            manager.get_flag_snapshot("com.android.aconfig.storage.test_1", "disabled_rw").unwrap();

        let expected_flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_1"),
            flag: String::from("disabled_rw"),
            server_value: String::from("false"),
            local_value: String::new(),
            boot_value: String::from("true"),
            default_value: String::from("false"),
            is_readwrite: true,
            has_server_override: true,
            has_local_override: false,
            has_boot_local_override: false,
        };

        assert_eq!(flag, Some(expected_flag));
    }

    #[test]
    fn test_remove_all_local_override() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);

        manager
            .override_flag_value(
                "com.android.aconfig.storage.test_1",
                "disabled_rw",
                "true",
                ProtoFlagOverrideType::LOCAL_ON_REBOOT,
            )
            .unwrap();

        manager
            .override_flag_value(
                "com.android.aconfig.storage.test_2",
                "disabled_rw",
                "true",
                ProtoFlagOverrideType::LOCAL_ON_REBOOT,
            )
            .unwrap();
        manager.apply_all_staged_overrides("mockup").unwrap();
        manager.remove_all_local_overrides().unwrap();

        let mut flag =
            manager.get_flag_snapshot("com.android.aconfig.storage.test_1", "disabled_rw").unwrap();

        let mut expected_flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_1"),
            flag: String::from("disabled_rw"),
            server_value: String::from(""),
            local_value: String::new(),
            boot_value: String::from("true"),
            default_value: String::from("false"),
            is_readwrite: true,
            has_server_override: false,
            has_local_override: false,
            has_boot_local_override: false,
        };

        assert_eq!(flag, Some(expected_flag));

        flag =
            manager.get_flag_snapshot("com.android.aconfig.storage.test_2", "disabled_rw").unwrap();

        expected_flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_2"),
            flag: String::from("disabled_rw"),
            server_value: String::from(""),
            local_value: String::new(),
            boot_value: String::from("true"),
            default_value: String::from("false"),
            is_readwrite: true,
            has_server_override: false,
            has_local_override: false,
            has_boot_local_override: false,
        };

        assert_eq!(flag, Some(expected_flag));
    }

    #[test]
    fn test_list_flags_in_package() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);
        add_example_overrides(&mut manager);
        manager.apply_all_staged_overrides("mockup").unwrap();

        let flags = manager.list_flags_in_package("com.android.aconfig.storage.test_1").unwrap();

        let mut flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_1"),
            flag: String::from("disabled_rw"),
            server_value: String::from("false"),
            local_value: String::from("true"),
            boot_value: String::from("true"),
            default_value: String::from("false"),
            is_readwrite: true,
            has_server_override: true,
            has_local_override: true,
            has_boot_local_override: false,
        };
        assert_eq!(flags[0], flag);

        flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_1"),
            flag: String::from("enabled_ro"),
            server_value: String::new(),
            local_value: String::new(),
            boot_value: String::from("true"),
            default_value: String::from("true"),
            is_readwrite: false,
            has_server_override: false,
            has_local_override: false,
            has_boot_local_override: false,
        };
        assert_eq!(flags[1], flag);

        flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_1"),
            flag: String::from("enabled_rw"),
            server_value: String::from("false"),
            local_value: String::new(),
            boot_value: String::from("false"),
            default_value: String::from("true"),
            is_readwrite: true,
            has_server_override: true,
            has_local_override: false,
            has_boot_local_override: false,
        };
        assert_eq!(flags[2], flag);
    }

    #[test]
    fn test_list_flags_in_container() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut manager = StorageFilesManager::new(&root_dir.tmp_dir.path());
        init_storage(&container, &mut manager);
        add_example_overrides(&mut manager);
        manager.apply_all_staged_overrides("mockup").unwrap();

        let flags = manager.list_flags_in_container("mockup").unwrap();
        assert_eq!(flags.len(), 8);

        let mut flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_1"),
            flag: String::from("enabled_rw"),
            server_value: String::from("false"),
            local_value: String::new(),
            boot_value: String::from("false"),
            default_value: String::from("true"),
            is_readwrite: true,
            has_server_override: true,
            has_local_override: false,
            has_boot_local_override: false,
        };
        assert_eq!(flags[2], flag);

        flag = FlagSnapshot {
            container: String::from("mockup"),
            package: String::from("com.android.aconfig.storage.test_1"),
            flag: String::from("disabled_rw"),
            server_value: String::from("false"),
            local_value: String::from("true"),
            boot_value: String::from("true"),
            default_value: String::from("false"),
            is_readwrite: true,
            has_server_override: true,
            has_local_override: true,
            has_boot_local_override: false,
        };
        assert_eq!(flags[0], flag);
    }
}
