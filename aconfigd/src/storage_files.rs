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

use crate::utils::{
    copy_file, copy_file_without_fsync, get_files_digest, read_pb_from_file, remove_file,
    write_pb_to_file,
};
use crate::AconfigdError;
use aconfig_storage_file::{
    list_flags, list_flags_with_info, FlagInfoBit, FlagValueSummary, FlagValueType,
};
use aconfig_storage_read_api::{
    get_boolean_flag_value, get_flag_read_context, get_package_read_context,
    get_storage_file_version, map_file,
};
use aconfig_storage_write_api::{
    map_mutable_storage_file, set_boolean_flag_value, set_flag_has_local_override,
    set_flag_has_server_override,
};
use aconfigd_protos::{ProtoFlagOverride, ProtoLocalFlagOverrides, ProtoPersistStorageRecord};
use anyhow::anyhow;
use log::debug;
use memmap2::{Mmap, MmapMut};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// In memory data structure for storage file locations for each container
#[derive(PartialEq, Debug, Clone)]
pub(crate) struct StorageRecord {
    pub version: u32,
    pub container: String,            // container name
    pub default_package_map: PathBuf, // default package map file
    pub default_flag_map: PathBuf,    // default flag map file
    pub default_flag_val: PathBuf,    // default flag val file
    pub default_flag_info: PathBuf,   // default flag info file
    pub persist_package_map: PathBuf, // persist package.map file
    pub persist_flag_map: PathBuf,    // persist flag.map file
    pub persist_flag_val: PathBuf,    // persist flag.val file
    pub persist_flag_info: PathBuf,   // persist flag.info file
    pub local_overrides: PathBuf,     // local overrides pb file
    pub boot_flag_val: PathBuf,       // boot flag.val file
    pub boot_flag_info: PathBuf,      // boot flag.info file
    pub digest: String,               // hash for all default storage files
}

// Storage files for a particular container
#[derive(Debug)]
pub(crate) struct StorageFiles {
    pub storage_record: StorageRecord,
    pub package_map: Option<Mmap>,
    pub flag_map: Option<Mmap>,
    pub flag_val: Option<Mmap>,                  // default flag value file
    pub boot_flag_val: Option<Mmap>,             // boot flag value file
    pub boot_flag_info: Option<Mmap>,            // boot flag info file
    pub persist_flag_val: Option<MmapMut>,       // persist flag value file
    pub persist_flag_info: Option<MmapMut>,      // persist flag info file
    pub mutable_boot_flag_val: Option<MmapMut>,  // mutable boot flag value file
    pub mutable_boot_flag_info: Option<MmapMut>, // mutable boot flag info file
}

// Compare two options of mmap/mmapmut
fn same_mmap_contents<T: std::ops::Deref<Target = [u8]>>(
    opt_a: &Option<T>,
    opt_b: &Option<T>,
) -> bool {
    match (opt_a, opt_b) {
        (Some(map_a), Some(map_b)) => map_a[..] == map_b[..],
        (None, None) => true,
        _ => false,
    }
}

impl PartialEq for StorageFiles {
    fn eq(&self, other: &Self) -> bool {
        self.storage_record == other.storage_record
            && same_mmap_contents(&self.package_map, &other.package_map)
            && same_mmap_contents(&self.flag_map, &other.flag_map)
            && same_mmap_contents(&self.flag_val, &other.flag_val)
            && same_mmap_contents(&self.boot_flag_val, &other.boot_flag_val)
            && same_mmap_contents(&self.boot_flag_info, &other.boot_flag_info)
            && same_mmap_contents(&self.persist_flag_val, &other.persist_flag_val)
            && same_mmap_contents(&self.persist_flag_info, &other.persist_flag_info)
            && same_mmap_contents(&self.mutable_boot_flag_val, &other.mutable_boot_flag_val)
            && same_mmap_contents(&self.mutable_boot_flag_info, &other.mutable_boot_flag_info)
    }
}

// Package and flag query context
#[derive(PartialEq, Debug)]
pub(crate) struct PackageFlagContext {
    pub package: String,
    pub flag: String,
    pub package_exists: bool,
    pub flag_exists: bool,
    pub value_type: FlagValueType,
    pub flag_index: u32,
}

// Flag snapshot in storage
#[derive(PartialEq, Debug)]
pub(crate) struct FlagSnapshot {
    pub container: String,
    pub package: String,
    pub flag: String,
    pub server_value: String,
    pub local_value: String,
    pub boot_value: String,
    pub default_value: String,
    pub is_readwrite: bool,
    pub has_server_override: bool,
    pub has_local_override: bool,
    pub has_boot_local_override: bool,
}

impl StorageFiles {
    /// Constructor from a container
    pub(crate) fn from_container(
        container: &str,
        package_map: &Path,
        flag_map: &Path,
        flag_val: &Path,
        flag_info: &Path,
        root_dir: &Path,
    ) -> Result<Self, AconfigdError> {
        debug!("create storage files object from container {}", container);
        let version =
            get_storage_file_version(&flag_val.display().to_string()).map_err(|errmsg| {
                AconfigdError::FailToGetFileVersion { file: flag_val.display().to_string(), errmsg }
            })?;

        let record = StorageRecord {
            version,
            container: container.to_string(),
            default_package_map: package_map.to_path_buf(),
            default_flag_map: flag_map.to_path_buf(),
            default_flag_val: flag_val.to_path_buf(),
            default_flag_info: flag_info.to_path_buf(),
            persist_package_map: root_dir.join("maps").join(container.to_string() + ".package.map"),
            persist_flag_map: root_dir.join("maps").join(container.to_string() + ".flag.map"),
            persist_flag_val: root_dir.join("flags").join(container.to_string() + ".val"),
            persist_flag_info: root_dir.join("flags").join(container.to_string() + ".info"),
            local_overrides: root_dir
                .join("flags")
                .join(container.to_string() + "_local_overrides.pb"),
            boot_flag_val: root_dir.join("boot").join(container.to_string() + ".val"),
            boot_flag_info: root_dir.join("boot").join(container.to_string() + ".info"),
            digest: get_files_digest(&[package_map, flag_map, flag_val, flag_info][..])?,
        };

        debug!("copy {} storage files to persist and boot directories", container);
        copy_file(package_map, &record.persist_package_map, 0o444)?;
        copy_file(flag_map, &record.persist_flag_map, 0o444)?;
        copy_file(flag_val, &record.persist_flag_val, 0o644)?;
        copy_file(flag_info, &record.persist_flag_info, 0o644)?;
        copy_file(flag_val, &record.boot_flag_val, 0o644)?;
        copy_file(flag_info, &record.boot_flag_info, 0o644)?;

        let pb = ProtoLocalFlagOverrides::new();
        write_pb_to_file::<ProtoLocalFlagOverrides>(&pb, &record.local_overrides)?;

        let files = Self {
            storage_record: record,
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

        Ok(files)
    }

    /// Constructor from a pb record
    pub(crate) fn from_pb(
        pb: &ProtoPersistStorageRecord,
        root_dir: &Path,
    ) -> Result<Self, AconfigdError> {
        debug!("create {} storage files object from pb entry", pb.container());
        let record = StorageRecord {
            version: pb.version(),
            container: pb.container().to_string(),
            default_package_map: PathBuf::from(pb.package_map()),
            default_flag_map: PathBuf::from(pb.flag_map()),
            default_flag_val: PathBuf::from(pb.flag_val()),
            default_flag_info: PathBuf::from(pb.flag_info()),
            persist_package_map: root_dir
                .join("maps")
                .join(pb.container().to_string() + ".package.map"),
            persist_flag_map: root_dir.join("maps").join(pb.container().to_string() + ".flag.map"),
            persist_flag_val: root_dir.join("flags").join(pb.container().to_string() + ".val"),
            persist_flag_info: root_dir.join("flags").join(pb.container().to_string() + ".info"),
            local_overrides: root_dir
                .join("flags")
                .join(pb.container().to_string() + "_local_overrides.pb"),
            boot_flag_val: root_dir.join("boot").join(pb.container().to_string() + ".val"),
            boot_flag_info: root_dir.join("boot").join(pb.container().to_string() + ".info"),
            digest: pb.digest().to_string(),
        };

        Ok(Self {
            storage_record: record,
            package_map: None,
            flag_map: None,
            flag_val: None,
            boot_flag_val: None,
            boot_flag_info: None,
            persist_flag_val: None,
            persist_flag_info: None,
            mutable_boot_flag_val: None,
            mutable_boot_flag_info: None,
        })
    }

    /// Get immutable file mapping of a file.
    ///
    /// # Safety
    ///
    /// The memory mapped file may have undefined behavior if there are writes to the underlying
    /// file after being mapped. Ensure no writes can happen to the underlying file that is memory
    /// mapped while this mapping stays alive to guarantee safety.
    unsafe fn get_immutable_file_mapping(file_path: &Path) -> Result<Mmap, AconfigdError> {
        // SAFETY: As per the safety comment, there are no other writes to the underlying file.
        unsafe {
            map_file(&file_path.display().to_string()).map_err(|errmsg| {
                AconfigdError::FailToMapFile { file: file_path.display().to_string(), errmsg }
            })
        }
    }

    /// Get package map memory mapping.
    fn get_package_map(&mut self) -> Result<&Mmap, AconfigdError> {
        if self.package_map.is_none() {
            // SAFETY: Here it is safe as package map files are always read only.
            self.package_map = unsafe {
                Some(Self::get_immutable_file_mapping(&self.storage_record.persist_package_map)?)
            };
        }
        Ok(self.package_map.as_ref().unwrap())
    }

    /// Get flag map memory mapping.
    fn get_flag_map(&mut self) -> Result<&Mmap, AconfigdError> {
        if self.flag_map.is_none() {
            // SAFETY: Here it is safe as flag map files are always read only.
            self.flag_map = unsafe {
                Some(Self::get_immutable_file_mapping(&self.storage_record.persist_flag_map)?)
            };
        }
        Ok(self.flag_map.as_ref().unwrap())
    }

    /// Get default flag value memory mapping.
    fn get_flag_val(&mut self) -> Result<&Mmap, AconfigdError> {
        if self.flag_val.is_none() {
            // SAFETY: Here it is safe as default flag value files are always read only.
            self.flag_val = unsafe {
                Some(Self::get_immutable_file_mapping(&self.storage_record.default_flag_val)?)
            };
        }
        Ok(self.flag_val.as_ref().unwrap())
    }

    /// Get boot flag value memory mapping.
    ///
    /// # Safety
    ///
    /// The memory mapped file may have undefined behavior if there are writes to the underlying
    /// file after being mapped. Ensure no writes can happen to the underlying file that is memory
    /// mapped while this mapping stays alive to guarantee safety.
    unsafe fn get_boot_flag_val(&mut self) -> Result<&Mmap, AconfigdError> {
        if self.boot_flag_val.is_none() {
            // SAFETY: As per the safety comment, there are no other writes to the underlying file.
            self.boot_flag_val = unsafe {
                Some(Self::get_immutable_file_mapping(&self.storage_record.boot_flag_val)?)
            };
        }
        Ok(self.boot_flag_val.as_ref().unwrap())
    }

    /// Get boot flag info memory mapping.
    ///
    /// # Safety
    ///
    /// The memory mapped file may have undefined behavior if there are writes to the underlying
    /// file after being mapped. Ensure no writes can happen to the underlying file that is memory
    /// mapped while this mapping stays alive to guarantee safety.
    unsafe fn get_boot_flag_info(&mut self) -> Result<&Mmap, AconfigdError> {
        if self.boot_flag_info.is_none() {
            // SAFETY: As per the safety comment, there are no other writes to the underlying file.
            self.boot_flag_info = unsafe {
                Some(Self::get_immutable_file_mapping(&self.storage_record.boot_flag_info)?)
            };
        }
        Ok(self.boot_flag_info.as_ref().unwrap())
    }

    /// Get mutable file mapping of a file.
    ///
    /// # Safety
    ///
    /// The memory mapped file may have undefined behavior if there are writes to this
    /// file not thru this memory mapped file or there are concurrent writes to this
    /// memory mapped file. Ensure all writes to the underlying file are thru this memory
    /// mapped file and there are no concurrent writes.
    pub(crate) unsafe fn get_mutable_file_mapping(
        file_path: &Path,
    ) -> Result<MmapMut, AconfigdError> {
        // SAFETY: As per the safety comment, there are no other writes to the underlying file.
        unsafe {
            map_mutable_storage_file(&file_path.display().to_string()).map_err(|errmsg| {
                AconfigdError::FailToMapFile { file: file_path.display().to_string(), errmsg }
            })
        }
    }

    /// Get persist flag value memory mapping.
    fn get_persist_flag_val(&mut self) -> Result<&mut MmapMut, AconfigdError> {
        if self.persist_flag_val.is_none() {
            // SAFETY: safety is ensured that all writes to the persist file is thru this
            // memory mapping, and there are no concurrent writes
            self.persist_flag_val = unsafe {
                Some(Self::get_mutable_file_mapping(&self.storage_record.persist_flag_val)?)
            };
        }
        Ok(self.persist_flag_val.as_mut().unwrap())
    }

    /// Get persist flag info memory mapping.
    fn get_persist_flag_info(&mut self) -> Result<&mut MmapMut, AconfigdError> {
        if self.persist_flag_info.is_none() {
            // SAFETY: safety is ensured that all writes to the persist file is thru this
            // memory mapping, and there are no concurrent writes
            self.persist_flag_info = unsafe {
                Some(Self::get_mutable_file_mapping(&self.storage_record.persist_flag_info)?)
            };
        }
        Ok(self.persist_flag_info.as_mut().unwrap())
    }

    /// Get mutable boot flag value memory mapping.
    fn get_mutable_boot_flag_val(&mut self) -> Result<&mut MmapMut, AconfigdError> {
        if self.mutable_boot_flag_val.is_none() {
            // SAFETY: safety is ensured that all writes to the persist file is thru this
            // memory mapping, and there are no concurrent writes
            self.mutable_boot_flag_val = unsafe {
                Some(Self::get_mutable_file_mapping(&self.storage_record.boot_flag_val)?)
            };
        }
        Ok(self.mutable_boot_flag_val.as_mut().unwrap())
    }

    /// Get mutable boot flag info memory mapping.
    fn get_mutable_boot_flag_info(&mut self) -> Result<&mut MmapMut, AconfigdError> {
        if self.mutable_boot_flag_info.is_none() {
            // SAFETY: safety is ensured that all writes to the persist file is thru this
            // memory mapping, and there are no concurrent writes
            self.mutable_boot_flag_info = unsafe {
                Some(Self::get_mutable_file_mapping(&self.storage_record.boot_flag_info)?)
            };
        }
        Ok(self.mutable_boot_flag_info.as_mut().unwrap())
    }

    /// Get package and flag query context
    pub(crate) fn get_package_flag_context(
        &mut self,
        package: &str,
        flag: &str,
    ) -> Result<PackageFlagContext, AconfigdError> {
        let mut context = PackageFlagContext {
            package: package.to_string(),
            flag: flag.to_string(),
            package_exists: false,
            flag_exists: false,
            value_type: FlagValueType::Boolean,
            flag_index: 0,
        };

        if package.is_empty() {
            return Ok(context);
        }

        let package_context =
            get_package_read_context(self.get_package_map()?, package).map_err(|errmsg| {
                AconfigdError::FailToGetPackageContext { package: package.to_string(), errmsg }
            })?;

        if let Some(pkg) = package_context {
            context.package_exists = true;
            if flag.is_empty() {
                return Ok(context);
            }

            let flag_context = get_flag_read_context(self.get_flag_map()?, pkg.package_id, flag)
                .map_err(|errmsg| AconfigdError::FailToGetFlagContext {
                    flag: package.to_string() + "." + flag,
                    errmsg,
                })?;

            if let Some(flg) = flag_context {
                context.flag_exists = true;
                context.value_type = FlagValueType::try_from(flg.flag_type).map_err(|errmsg| {
                    AconfigdError::InvalidFlagValueType {
                        flag: package.to_string() + "." + flag,
                        errmsg,
                    }
                })?;
                context.flag_index = pkg.boolean_start_index + flg.flag_index as u32;
            }
        } else {
            debug!(
                "failed to find package {} in container {}",
                package, self.storage_record.container
            );
        }

        Ok(context)
    }

    /// Check if has an aconfig package
    pub(crate) fn has_package(&mut self, package: &str) -> Result<bool, AconfigdError> {
        let context = self.get_package_flag_context(package, "")?;
        Ok(context.package_exists)
    }

    /// Get flag attribute bitfield
    pub(crate) fn get_flag_attribute(
        &mut self,
        context: &PackageFlagContext,
    ) -> Result<u8, AconfigdError> {
        if !context.flag_exists {
            return Err(AconfigdError::FlagDoesNotExist {
                flag: context.package.to_string() + "." + &context.flag,
            });
        }

        let flag_info_file = self.get_persist_flag_info()?;
        Ok(aconfig_storage_read_api::get_flag_attribute(
            flag_info_file,
            context.value_type,
            context.flag_index,
        )
        .map_err(|errmsg| AconfigdError::FailToGetFlagAttribute {
            flag: context.package.to_string() + "." + &context.flag,
            errmsg,
        })?)
    }

    /// Get flag value from a mapped file
    fn get_flag_value_from_file(
        file: &[u8],
        context: &PackageFlagContext,
    ) -> Result<String, AconfigdError> {
        if !context.flag_exists {
            return Err(AconfigdError::FlagDoesNotExist {
                flag: context.package.to_string() + "." + &context.flag,
            });
        }

        match context.value_type {
            FlagValueType::Boolean => {
                let value = get_boolean_flag_value(file, context.flag_index).map_err(|errmsg| {
                    AconfigdError::FailToGetFlagValue {
                        flag: context.package.to_string() + "." + &context.flag,
                        errmsg,
                    }
                })?;
                if value {
                    Ok(String::from("true"))
                } else {
                    Ok(String::from("false"))
                }
            }
        }
    }

    /// Get server flag value
    pub(crate) fn get_server_flag_value(
        &mut self,
        context: &PackageFlagContext,
    ) -> Result<String, AconfigdError> {
        let attribute = self.get_flag_attribute(context)?;
        if (attribute & FlagInfoBit::HasServerOverride as u8) == 0 {
            return Ok(String::new());
        }

        let flag_val_file = self.get_persist_flag_val()?;
        Self::get_flag_value_from_file(flag_val_file, context)
    }

    /// Get boot flag value
    pub(crate) fn get_boot_flag_value(
        &mut self,
        context: &PackageFlagContext,
    ) -> Result<String, AconfigdError> {
        // SAFETY: safety is ensured as we are only read from the memory mapping
        let flag_val_file = unsafe { self.get_boot_flag_val()? };
        Self::get_flag_value_from_file(flag_val_file, context)
    }

    /// Get default flag value
    pub(crate) fn get_default_flag_value(
        &mut self,
        context: &PackageFlagContext,
    ) -> Result<String, AconfigdError> {
        let flag_val_file = self.get_flag_val()?;
        Self::get_flag_value_from_file(flag_val_file, context)
    }

    /// Get local flag value
    pub(crate) fn get_local_flag_value(
        &mut self,
        context: &PackageFlagContext,
    ) -> Result<String, AconfigdError> {
        let attribute = self.get_flag_attribute(context)?;
        if (attribute & FlagInfoBit::HasLocalOverride as u8) == 0 {
            return Ok(String::new());
        }

        let pb =
            read_pb_from_file::<ProtoLocalFlagOverrides>(&self.storage_record.local_overrides)?;

        for entry in pb.overrides {
            if entry.package_name() == context.package && entry.flag_name() == context.flag {
                return Ok(String::from(entry.flag_value()));
            }
        }

        Err(AconfigdError::FlagHasNoLocalOverride {
            flag: context.package.to_string() + "." + &context.flag,
        })
    }

    /// Set flag value to file
    pub(crate) fn set_flag_value_to_file(
        file: &mut MmapMut,
        context: &PackageFlagContext,
        value: &str,
    ) -> Result<(), AconfigdError> {
        match context.value_type {
            FlagValueType::Boolean => {
                if value != "true" && value != "false" {
                    return Err(AconfigdError::InvalidFlagValue {
                        flag: context.package.to_string() + "." + &context.flag,
                        value: value.to_string(),
                    });
                }
                set_boolean_flag_value(file, context.flag_index, value == "true").map_err(
                    |errmsg| AconfigdError::FailToSetFlagValue {
                        flag: context.package.to_string() + "." + &context.flag,
                        errmsg,
                    },
                )?;
            }
        }

        Ok(())
    }

    /// Set flag has server override to file
    fn set_flag_has_server_override_to_file(
        file: &mut MmapMut,
        context: &PackageFlagContext,
        value: bool,
    ) -> Result<(), AconfigdError> {
        set_flag_has_server_override(file, context.value_type, context.flag_index, value).map_err(
            |errmsg| AconfigdError::FailToSetFlagHasServerOverride {
                flag: context.package.to_string() + "." + &context.flag,
                errmsg,
            },
        )?;

        Ok(())
    }

    /// Set flag has local override to file
    pub(crate) fn set_flag_has_local_override_to_file(
        file: &mut MmapMut,
        context: &PackageFlagContext,
        value: bool,
    ) -> Result<(), AconfigdError> {
        set_flag_has_local_override(file, context.value_type, context.flag_index, value).map_err(
            |errmsg| AconfigdError::FailToSetFlagHasLocalOverride {
                flag: context.package.to_string() + "." + &context.flag,
                errmsg,
            },
        )?;

        Ok(())
    }

    /// Server override a flag
    pub(crate) fn stage_server_override(
        &mut self,
        context: &PackageFlagContext,
        value: &str,
    ) -> Result<(), AconfigdError> {
        debug!(
            "staging server override for flag {} with value {}",
            context.package.to_string() + "." + &context.flag,
            value
        );
        let attribute = self.get_flag_attribute(context)?;
        if (attribute & FlagInfoBit::IsReadWrite as u8) == 0 {
            return Err(AconfigdError::FlagIsReadOnly {
                flag: context.package.to_string() + "." + &context.flag,
            });
        }

        let flag_val_file = self.get_persist_flag_val()?;
        Self::set_flag_value_to_file(flag_val_file, context, value)?;

        let flag_info_file = self.get_persist_flag_info()?;
        Self::set_flag_has_server_override_to_file(flag_info_file, context, true)?;

        Ok(())
    }

    /// Stage local override of a flag
    pub(crate) fn stage_local_override(
        &mut self,
        context: &PackageFlagContext,
        value: &str,
    ) -> Result<(), AconfigdError> {
        debug!(
            "staging local override for flag {} with value {}",
            context.package.to_string() + "." + &context.flag,
            value
        );
        let attribute = self.get_flag_attribute(context)?;
        if (attribute & FlagInfoBit::IsReadWrite as u8) == 0 {
            return Err(AconfigdError::FlagIsReadOnly {
                flag: context.package.to_string() + "." + &context.flag,
            });
        }

        let mut exist = false;
        let mut pb =
            read_pb_from_file::<ProtoLocalFlagOverrides>(&self.storage_record.local_overrides)?;
        for entry in &mut pb.overrides {
            if entry.package_name() == context.package && entry.flag_name() == context.flag {
                entry.set_flag_value(String::from(value));
                exist = true;
                break;
            }
        }
        if !exist {
            let mut new_entry = ProtoFlagOverride::new();
            new_entry.set_package_name(context.package.clone());
            new_entry.set_flag_name(context.flag.clone());
            new_entry.set_flag_value(String::from(value));
            pb.overrides.push(new_entry);
        }

        write_pb_to_file::<ProtoLocalFlagOverrides>(&pb, &self.storage_record.local_overrides)?;

        let flag_info_file = self.get_persist_flag_info()?;
        Self::set_flag_has_local_override_to_file(flag_info_file, context, true)?;

        Ok(())
    }

    /// Stage and apply local override of a flag
    pub(crate) fn stage_and_apply_local_override(
        &mut self,
        context: &PackageFlagContext,
        value: &str,
    ) -> Result<(), AconfigdError> {
        self.stage_local_override(&context, value)?;

        debug!(
            "apply local override for flag {} with value {}",
            context.package.to_string() + "." + &context.flag,
            value
        );
        let mut mut_boot_flag_val = self.get_mutable_boot_flag_val()?;
        Self::set_flag_value_to_file(&mut mut_boot_flag_val, &context, value)?;
        let mut mut_boot_flag_info = self.get_mutable_boot_flag_info()?;
        Self::set_flag_has_local_override_to_file(&mut mut_boot_flag_info, &context, true)?;
        Ok(())
    }

    /// Apply all staged local overrides
    fn apply_staged_local_overrides(&mut self) -> Result<(), AconfigdError> {
        debug!("apply staged local overrides for container {}", &self.storage_record.container);
        let pb =
            read_pb_from_file::<ProtoLocalFlagOverrides>(&self.storage_record.local_overrides)?;

        for entry in pb.overrides {
            let context = self.get_package_flag_context(entry.package_name(), entry.flag_name())?;
            let mut flag_val_file = self.get_mutable_boot_flag_val()?;
            Self::set_flag_value_to_file(&mut flag_val_file, &context, entry.flag_value())?;
        }

        Ok(())
    }

    /// Apply both server and local overrides
    pub(crate) fn apply_all_staged_overrides(&mut self) -> Result<(), AconfigdError> {
        debug!("apply staged server overrides for container {}", &self.storage_record.container);
        copy_file_without_fsync(
            &self.storage_record.persist_flag_val,
            &self.storage_record.boot_flag_val,
            0o644,
        )?;
        copy_file_without_fsync(
            &self.storage_record.persist_flag_info,
            &self.storage_record.boot_flag_info,
            0o644,
        )?;
        self.apply_staged_local_overrides()?;
        Ok(())
    }

    /// Get all current server overrides
    pub(crate) fn get_all_server_overrides(
        &mut self,
    ) -> Result<Vec<FlagValueSummary>, AconfigdError> {
        debug!("get all staged server overrides for container {}", &self.storage_record.container);
        let listed_flags = list_flags_with_info(
            &self.storage_record.persist_package_map.display().to_string(),
            &self.storage_record.persist_flag_map.display().to_string(),
            &self.storage_record.persist_flag_val.display().to_string(),
            &self.storage_record.persist_flag_info.display().to_string(),
        )
        .map_err(|errmsg| AconfigdError::FailToListFlagsWithInfo {
            container: self.storage_record.container.clone(),
            errmsg,
        })?;

        Ok(listed_flags
            .into_iter()
            .filter(|f| f.has_server_override)
            .map(|f| FlagValueSummary {
                package_name: f.package_name,
                flag_name: f.flag_name,
                flag_value: f.flag_value,
                value_type: f.value_type,
            })
            .collect())
    }

    /// Get all local overrides
    pub(crate) fn get_all_local_overrides(
        &mut self,
    ) -> Result<Vec<ProtoFlagOverride>, AconfigdError> {
        debug!("get all staged local overrides for container {}", &self.storage_record.container);
        let pb =
            read_pb_from_file::<ProtoLocalFlagOverrides>(&self.storage_record.local_overrides)?;
        Ok(pb.overrides)
    }

    /// Remove a local flag override
    pub(crate) fn remove_local_override(
        &mut self,
        context: &PackageFlagContext,
        immediate: bool,
    ) -> Result<(), AconfigdError> {
        debug!(
            "remove local override for flag {}",
            context.package.to_string() + "." + &context.flag
        );
        let attribute = self.get_flag_attribute(context)?;
        if (attribute & FlagInfoBit::HasLocalOverride as u8) == 0 {
            return Err(AconfigdError::FlagHasNoLocalOverride {
                flag: context.package.to_string() + "." + &context.flag,
            });
        }

        let mut pb =
            read_pb_from_file::<ProtoLocalFlagOverrides>(&self.storage_record.local_overrides)?;
        pb.overrides = pb
            .overrides
            .into_iter()
            .filter(|f| f.package_name() != context.package || f.flag_name() != context.flag)
            .collect();
        write_pb_to_file::<ProtoLocalFlagOverrides>(&pb, &self.storage_record.local_overrides)?;

        let flag_info_file = self.get_persist_flag_info()?;
        Self::set_flag_has_local_override_to_file(flag_info_file, context, false)?;

        if configinfra_framework_flags_rust::enable_immediate_clear_override_bugfix() && immediate {
            let value = if (attribute & FlagInfoBit::HasServerOverride as u8) == 1 {
                self.get_server_flag_value(&context)?
            } else {
                self.get_default_flag_value(&context)?
            };

            let mut mut_boot_flag_val = self.get_mutable_boot_flag_val()?;
            Self::set_flag_value_to_file(&mut mut_boot_flag_val, &context, &value)?;

            let mut mut_boot_flag_info = self.get_mutable_boot_flag_info()?;
            Self::set_flag_has_local_override_to_file(&mut mut_boot_flag_info, &context, false)?;
        }

        Ok(())
    }

    /// Remove all local flag overrides
    pub(crate) fn remove_all_local_overrides(&mut self) -> Result<(), AconfigdError> {
        debug!("remove all local overrides for container {}", &self.storage_record.container);
        let pb =
            read_pb_from_file::<ProtoLocalFlagOverrides>(&self.storage_record.local_overrides)?;

        for entry in pb.overrides {
            let context = self.get_package_flag_context(entry.package_name(), entry.flag_name())?;
            let attribute = self.get_flag_attribute(&context)?;
            if (attribute & FlagInfoBit::HasLocalOverride as u8) == 0 {
                return Err(AconfigdError::FlagHasNoLocalOverride {
                    flag: context.package.to_string() + "." + &context.flag,
                });
            }

            let flag_info_file = self.get_persist_flag_info()?;
            Self::set_flag_has_local_override_to_file(flag_info_file, &context, false)?;
        }

        write_pb_to_file::<ProtoLocalFlagOverrides>(
            &ProtoLocalFlagOverrides::new(),
            &self.storage_record.local_overrides,
        )?;

        Ok(())
    }

    /// Clean up, it cannot be implemented as the drop trait as it needs to return a Result
    pub(crate) fn remove_persist_files(&mut self) -> Result<(), AconfigdError> {
        debug!(
            "remove all persistent storage files for container {}",
            &self.storage_record.container
        );
        remove_file(&self.storage_record.persist_package_map)?;
        remove_file(&self.storage_record.persist_flag_map)?;
        remove_file(&self.storage_record.persist_flag_val)?;
        remove_file(&self.storage_record.persist_flag_info)?;
        remove_file(&self.storage_record.local_overrides)
    }

    /// get flag snapshot
    pub(crate) fn get_flag_snapshot(
        &mut self,
        package: &str,
        flag: &str,
    ) -> Result<Option<FlagSnapshot>, AconfigdError> {
        let context = self.get_package_flag_context(package, flag)?;
        if !context.flag_exists {
            return Ok(None);
        }

        let attribute = self.get_flag_attribute(&context)?;
        let server_value = self.get_server_flag_value(&context)?;
        let local_value = self.get_local_flag_value(&context)?;
        let boot_value = self.get_boot_flag_value(&context)?;
        let default_value = self.get_default_flag_value(&context)?;

        Ok(Some(FlagSnapshot {
            container: self.storage_record.container.clone(),
            package: package.to_string(),
            flag: flag.to_string(),
            server_value,
            local_value,
            boot_value,
            default_value,
            is_readwrite: attribute & FlagInfoBit::IsReadWrite as u8 != 0,
            has_server_override: attribute & FlagInfoBit::HasServerOverride as u8 != 0,
            has_local_override: attribute & FlagInfoBit::HasLocalOverride as u8 != 0,
            has_boot_local_override: false, // This is unsupported for get_flag_snapshot.
        }))
    }

    /// list flags in a package
    pub(crate) fn list_flags_in_package(
        &mut self,
        package: &str,
    ) -> Result<Vec<FlagSnapshot>, AconfigdError> {
        debug!("list all flags in package {}", &package);
        if !self.has_package(package)? {
            return Ok(Vec::new());
        }

        let mut snapshots: Vec<_> = list_flags_with_info(
            &self.storage_record.persist_package_map.display().to_string(),
            &self.storage_record.persist_flag_map.display().to_string(),
            &self.storage_record.persist_flag_val.display().to_string(),
            &self.storage_record.persist_flag_info.display().to_string(),
        )
        .map_err(|errmsg| AconfigdError::FailToListFlagsWithInfo {
            container: self.storage_record.container.clone(),
            errmsg,
        })?
        .into_iter()
        .filter(|f| f.package_name == package)
        .map(|f| FlagSnapshot {
            container: self.storage_record.container.clone(),
            package: f.package_name.clone(),
            flag: f.flag_name.clone(),
            server_value: if f.has_server_override { f.flag_value.clone() } else { String::new() },
            local_value: String::new(),
            boot_value: String::new(),
            default_value: String::new(),
            is_readwrite: f.is_readwrite,
            has_server_override: f.has_server_override,
            has_local_override: f.has_local_override,
            has_boot_local_override: false, // Placeholder; this is mutated and set below.
        })
        .collect();

        let mut flag_index = HashMap::new();
        for (i, f) in snapshots.iter().enumerate() {
            flag_index.insert(f.package.clone() + "/" + &f.flag, i);
        }

        let mut flags: Vec<_> = list_flags_with_info(
            &self.storage_record.persist_package_map.display().to_string(),
            &self.storage_record.persist_flag_map.display().to_string(),
            &self.storage_record.boot_flag_val.display().to_string(),
            &self.storage_record.boot_flag_info.display().to_string(),
        )
        .map_err(|errmsg| AconfigdError::FailToListFlags {
            container: self.storage_record.container.clone(),
            errmsg,
        })?
        .into_iter()
        .filter(|f| f.package_name == package)
        .collect();

        for f in flags.iter() {
            let full_flag_name = f.package_name.clone() + "/" + &f.flag_name;
            let index =
                flag_index.get(&full_flag_name).ok_or(AconfigdError::InternalError(anyhow!(
                    "Flag {}.{} appears in boot files but not in persist fliles",
                    &f.package_name,
                    &f.flag_name,
                )))?;
            snapshots[*index].boot_value = f.flag_value.clone();
            snapshots[*index].has_boot_local_override = f.has_local_override;
        }

        let flags: Vec<_> = list_flags(
            &self.storage_record.persist_package_map.display().to_string(),
            &self.storage_record.persist_flag_map.display().to_string(),
            &self.storage_record.default_flag_val.display().to_string(),
        )
        .map_err(|errmsg| AconfigdError::FailToListFlags {
            container: self.storage_record.container.clone(),
            errmsg,
        })?
        .into_iter()
        .filter(|f| f.package_name == package)
        .collect();

        for f in flags.iter() {
            let full_flag_name = f.package_name.clone() + "/" + &f.flag_name;
            let index =
                flag_index.get(&full_flag_name).ok_or(AconfigdError::InternalError(anyhow!(
                    "Flag {}.{} appears in default files but not in persist fliles",
                    &f.package_name,
                    &f.flag_name,
                )))?;
            snapshots[*index].default_value = f.flag_value.clone();
        }

        let pb =
            read_pb_from_file::<ProtoLocalFlagOverrides>(&self.storage_record.local_overrides)?;

        for entry in pb.overrides {
            let full_flag_name = entry.package_name().to_string() + "/" + entry.flag_name();
            if let Some(index) = flag_index.get(&full_flag_name) {
                snapshots[*index].local_value = entry.flag_value().to_string();
            }
        }

        Ok(snapshots)
    }

    /// list all flags in a container
    pub(crate) fn list_all_flags(&mut self) -> Result<Vec<FlagSnapshot>, AconfigdError> {
        debug!("list all flags in container {}", &self.storage_record.container);
        let mut snapshots: Vec<_> = list_flags_with_info(
            &self.storage_record.persist_package_map.display().to_string(),
            &self.storage_record.persist_flag_map.display().to_string(),
            &self.storage_record.persist_flag_val.display().to_string(),
            &self.storage_record.persist_flag_info.display().to_string(),
        )
        .map_err(|errmsg| AconfigdError::FailToListFlagsWithInfo {
            container: self.storage_record.container.clone(),
            errmsg,
        })?
        .into_iter()
        .map(|f| FlagSnapshot {
            container: self.storage_record.container.clone(),
            package: f.package_name.clone(),
            flag: f.flag_name.clone(),
            server_value: if f.has_server_override { f.flag_value.clone() } else { String::new() },
            local_value: String::new(),
            boot_value: String::new(),
            default_value: String::new(),
            is_readwrite: f.is_readwrite,
            has_server_override: f.has_server_override,
            has_local_override: f.has_local_override,
            has_boot_local_override: false, // Placeholder value; this is mutated and set below.
        })
        .collect();

        let mut flag_index = HashMap::new();
        for (i, f) in snapshots.iter().enumerate() {
            flag_index.insert(f.package.clone() + "/" + &f.flag, i);
        }

        let mut flags: Vec<_> = list_flags_with_info(
            &self.storage_record.persist_package_map.display().to_string(),
            &self.storage_record.persist_flag_map.display().to_string(),
            &self.storage_record.boot_flag_val.display().to_string(),
            &self.storage_record.boot_flag_info.display().to_string(),
        )
        .map_err(|errmsg| AconfigdError::FailToListFlags {
            container: self.storage_record.container.clone(),
            errmsg,
        })?
        .into_iter()
        .collect();

        for f in flags.iter() {
            let full_flag_name = f.package_name.clone() + "/" + &f.flag_name;
            let index =
                flag_index.get(&full_flag_name).ok_or(AconfigdError::InternalError(anyhow!(
                    "Flag {}.{} appears in boot files but not in persist fliles",
                    &f.package_name,
                    &f.flag_name,
                )))?;
            snapshots[*index].boot_value = f.flag_value.clone();
            snapshots[*index].has_boot_local_override = f.has_local_override;
        }

        let flags: Vec<_> = list_flags(
            &self.storage_record.persist_package_map.display().to_string(),
            &self.storage_record.persist_flag_map.display().to_string(),
            &self.storage_record.default_flag_val.display().to_string(),
        )
        .map_err(|errmsg| AconfigdError::FailToListFlags {
            container: self.storage_record.container.clone(),
            errmsg,
        })?
        .into_iter()
        .collect();

        for f in flags.iter() {
            let full_flag_name = f.package_name.clone() + "/" + &f.flag_name;
            let index =
                flag_index.get(&full_flag_name).ok_or(AconfigdError::InternalError(anyhow!(
                    "Flag {}.{} appears in default files but not in persist fliles",
                    &f.package_name,
                    &f.flag_name,
                )))?;
            snapshots[*index].default_value = f.flag_value.clone();
        }

        let pb =
            read_pb_from_file::<ProtoLocalFlagOverrides>(&self.storage_record.local_overrides)?;

        for entry in pb.overrides {
            let full_flag_name = entry.package_name().to_string() + "/" + entry.flag_name();
            if let Some(index) = flag_index.get(&full_flag_name) {
                snapshots[*index].local_value = entry.flag_value().to_string();
            }
        }

        Ok(snapshots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{has_same_content, ContainerMock, StorageRootDirMock};
    use aconfig_storage_file::StoredFlagType;

    fn create_mock_storage_files(
        container: &ContainerMock,
        root_dir: &StorageRootDirMock,
    ) -> StorageFiles {
        StorageFiles::from_container(
            &container.name,
            &container.package_map,
            &container.flag_map,
            &container.flag_val,
            &container.flag_info,
            &root_dir.tmp_dir.path(),
        )
        .unwrap()
    }

    #[test]
    fn test_create_storage_file_from_container() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let storage_files = create_mock_storage_files(&container, &root_dir);

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

        assert_eq!(storage_files, expected_storage_files);

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
        assert!(storage_files.storage_record.local_overrides.exists());
    }

    #[test]
    fn test_create_storage_file_from_pb() {
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

        let storage_files = StorageFiles::from_pb(&pb, &root_dir.tmp_dir.path()).unwrap();

        let expected_record = StorageRecord {
            version: 123,
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
            digest: String::from("abc"),
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

        assert_eq!(storage_files, expected_storage_files);
    }

    #[test]
    fn test_get_package_flag_context() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);

        let mut context = PackageFlagContext {
            package: String::from("not_exist"),
            flag: String::new(),
            package_exists: false,
            flag_exists: false,
            value_type: FlagValueType::Boolean,
            flag_index: 0,
        };
        let mut actual_context = storage_files.get_package_flag_context("not_exist", "").unwrap();
        assert_eq!(context, actual_context);

        context.package = String::from("com.android.aconfig.storage.test_1");
        context.package_exists = true;
        actual_context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "")
            .unwrap();
        assert_eq!(context, actual_context);

        context.flag = String::from("not_exist");
        actual_context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "not_exist")
            .unwrap();
        assert_eq!(context, actual_context);

        context.flag = String::from("enabled_rw");
        context.flag_exists = true;
        context.flag_index = 2;
        actual_context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        assert_eq!(context, actual_context);

        context.package = String::from("com.android.aconfig.storage.test_2");
        context.flag = String::from("disabled_rw");
        context.flag_index = 3;
        actual_context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_2", "disabled_rw")
            .unwrap();
        assert_eq!(context, actual_context);
    }

    #[test]
    fn test_has_package() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);
        assert!(!storage_files.has_package("not_exist").unwrap());
        assert!(storage_files.has_package("com.android.aconfig.storage.test_1").unwrap());
    }

    #[test]
    fn test_get_flag_attribute() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);
        let mut context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "not_exist")
            .unwrap();
        assert!(storage_files.get_flag_attribute(&context).is_err());

        context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        let attribute = storage_files.get_flag_attribute(&context).unwrap();
        assert!(attribute & (FlagInfoBit::IsReadWrite as u8) != 0);
        assert!(attribute & (FlagInfoBit::HasServerOverride as u8) == 0);
        assert!(attribute & (FlagInfoBit::HasLocalOverride as u8) == 0);
    }

    #[test]
    fn test_get_server_flag_value() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);
        let context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();

        assert_eq!(&storage_files.get_server_flag_value(&context).unwrap(), "");
        storage_files.stage_server_override(&context, "false").unwrap();
        assert_eq!(&storage_files.get_server_flag_value(&context).unwrap(), "false");
        storage_files.stage_server_override(&context, "true").unwrap();
        assert_eq!(&storage_files.get_server_flag_value(&context).unwrap(), "true");
    }

    #[test]
    fn test_get_boot_flag_value() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);
        let mut context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        assert_eq!(storage_files.get_boot_flag_value(&context).unwrap(), "true");
        context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_2", "disabled_rw")
            .unwrap();
        assert_eq!(storage_files.get_boot_flag_value(&context).unwrap(), "false");
    }

    #[test]
    fn test_get_default_flag_value() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);
        let mut context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        assert_eq!(storage_files.get_default_flag_value(&context).unwrap(), "true");
        context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_2", "disabled_rw")
            .unwrap();
        assert_eq!(storage_files.get_default_flag_value(&context).unwrap(), "false");
    }

    #[test]
    fn test_get_local_flag_value() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);
        let context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        assert_eq!(&storage_files.get_local_flag_value(&context).unwrap(), "");
        storage_files.stage_local_override(&context, "false").unwrap();
        assert_eq!(&storage_files.get_local_flag_value(&context).unwrap(), "false");
        storage_files.stage_local_override(&context, "true").unwrap();
        assert_eq!(&storage_files.get_local_flag_value(&context).unwrap(), "true");
    }

    #[test]
    fn test_stage_server_override() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);
        let context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        storage_files.stage_server_override(&context, "false").unwrap();
        assert_eq!(&storage_files.get_server_flag_value(&context).unwrap(), "false");
        let attribute = storage_files.get_flag_attribute(&context).unwrap();
        assert!(attribute & (FlagInfoBit::HasServerOverride as u8) != 0);
    }

    #[test]
    fn test_stage_local_override() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);
        let context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        storage_files.stage_local_override(&context, "false").unwrap();
        assert_eq!(&storage_files.get_local_flag_value(&context).unwrap(), "false");
        let attribute = storage_files.get_flag_attribute(&context).unwrap();
        assert!(attribute & (FlagInfoBit::HasLocalOverride as u8) != 0);
    }

    #[test]
    fn test_stage_and_apply_local_override() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);
        let context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        storage_files.stage_and_apply_local_override(&context, "false").unwrap();
        assert_eq!(&storage_files.get_local_flag_value(&context).unwrap(), "false");
        assert_eq!(&storage_files.get_boot_flag_value(&context).unwrap(), "false");
        let attribute = storage_files.get_flag_attribute(&context).unwrap();
        assert!(attribute & (FlagInfoBit::HasLocalOverride as u8) != 0);
    }

    #[test]
    fn test_apply_all_staged_overrides() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);

        let context_one = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        storage_files.stage_server_override(&context_one, "false").unwrap();

        let context_two = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_2", "disabled_rw")
            .unwrap();
        storage_files.stage_server_override(&context_two, "false").unwrap();
        storage_files.stage_local_override(&context_two, "true").unwrap();

        storage_files.apply_all_staged_overrides().unwrap();

        assert!(storage_files.storage_record.boot_flag_val.exists());
        assert!(storage_files.storage_record.boot_flag_info.exists());

        assert_eq!(storage_files.get_boot_flag_value(&context_one).unwrap(), "false");
        assert_eq!(storage_files.get_boot_flag_value(&context_two).unwrap(), "true");
    }

    #[test]
    fn test_get_all_server_overrides() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);
        let mut context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        storage_files.stage_server_override(&context, "false").unwrap();
        context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_2", "disabled_rw")
            .unwrap();
        storage_files.stage_server_override(&context, "true").unwrap();
        let server_overrides = storage_files.get_all_server_overrides().unwrap();
        assert_eq!(server_overrides.len(), 2);
        assert_eq!(
            server_overrides[0],
            FlagValueSummary {
                package_name: "com.android.aconfig.storage.test_1".to_string(),
                flag_name: "enabled_rw".to_string(),
                flag_value: "false".to_string(),
                value_type: StoredFlagType::ReadWriteBoolean,
            }
        );
        assert_eq!(
            server_overrides[1],
            FlagValueSummary {
                package_name: "com.android.aconfig.storage.test_2".to_string(),
                flag_name: "disabled_rw".to_string(),
                flag_value: "true".to_string(),
                value_type: StoredFlagType::ReadWriteBoolean,
            }
        );
    }

    #[test]
    fn test_get_all_local_overrides() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);

        let context_one = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        storage_files.stage_local_override(&context_one, "false").unwrap();

        let context_two = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_2", "disabled_rw")
            .unwrap();
        storage_files.stage_local_override(&context_two, "false").unwrap();

        let local_overrides = storage_files.get_all_local_overrides().unwrap();
        assert_eq!(local_overrides.len(), 2);

        let mut override_proto = ProtoFlagOverride::new();
        override_proto.set_package_name("com.android.aconfig.storage.test_1".to_string());
        override_proto.set_flag_name("enabled_rw".to_string());
        override_proto.set_flag_value("false".to_string());
        assert_eq!(local_overrides[0], override_proto);

        override_proto.set_package_name("com.android.aconfig.storage.test_2".to_string());
        override_proto.set_flag_name("disabled_rw".to_string());
        assert_eq!(local_overrides[1], override_proto);
    }

    #[test]
    fn test_remove_local_override() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);
        let context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();

        assert!(storage_files.remove_local_override(&context, false).is_err());
        storage_files.stage_local_override(&context, "false").unwrap();
        storage_files.remove_local_override(&context, false).unwrap();
        assert_eq!(&storage_files.get_local_flag_value(&context).unwrap(), "");
        let attribute = storage_files.get_flag_attribute(&context).unwrap();
        assert!(attribute & (FlagInfoBit::HasLocalOverride as u8) == 0);
    }

    #[test]
    fn test_remove_all_local_overrides() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);

        let context_one = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        storage_files.stage_local_override(&context_one, "false").unwrap();

        let context_two = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_2", "disabled_rw")
            .unwrap();
        storage_files.stage_local_override(&context_two, "false").unwrap();

        let mut pb = read_pb_from_file::<ProtoLocalFlagOverrides>(
            &storage_files.storage_record.local_overrides,
        )
        .unwrap();
        assert_eq!(pb.overrides.len(), 2);

        storage_files.remove_all_local_overrides().unwrap();

        assert_eq!(&storage_files.get_local_flag_value(&context_one).unwrap(), "");
        let mut attribute = storage_files.get_flag_attribute(&context_one).unwrap();
        assert!(attribute & (FlagInfoBit::HasLocalOverride as u8) == 0);

        assert_eq!(&storage_files.get_local_flag_value(&context_two).unwrap(), "");
        attribute = storage_files.get_flag_attribute(&context_one).unwrap();
        assert!(attribute & (FlagInfoBit::HasLocalOverride as u8) == 0);

        pb = read_pb_from_file::<ProtoLocalFlagOverrides>(
            &storage_files.storage_record.local_overrides,
        )
        .unwrap();
        assert_eq!(pb.overrides.len(), 0);
    }

    #[test]
    fn test_remove_persist_files() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);
        write_pb_to_file::<ProtoLocalFlagOverrides>(
            &ProtoLocalFlagOverrides::new(),
            &storage_files.storage_record.local_overrides,
        )
        .unwrap();
        assert!(storage_files.storage_record.persist_package_map.exists());
        assert!(storage_files.storage_record.persist_flag_map.exists());
        assert!(storage_files.storage_record.persist_flag_val.exists());
        assert!(storage_files.storage_record.persist_flag_info.exists());
        assert!(storage_files.storage_record.local_overrides.exists());

        storage_files.remove_persist_files().unwrap();
        assert!(!storage_files.storage_record.persist_package_map.exists());
        assert!(!storage_files.storage_record.persist_flag_map.exists());
        assert!(!storage_files.storage_record.persist_flag_val.exists());
        assert!(!storage_files.storage_record.persist_flag_info.exists());
        assert!(!storage_files.storage_record.local_overrides.exists());
    }

    #[test]
    fn test_get_flag_snapshot() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);

        let mut flag = storage_files
            .get_flag_snapshot("com.android.aconfig.storage.test_1", "not_exist")
            .unwrap();
        assert_eq!(flag, None);

        let context = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "disabled_rw")
            .unwrap();
        storage_files.stage_server_override(&context, "false").unwrap();
        storage_files.stage_local_override(&context, "true").unwrap();
        storage_files.apply_all_staged_overrides().unwrap();

        flag = storage_files
            .get_flag_snapshot("com.android.aconfig.storage.test_1", "disabled_rw")
            .unwrap();

        let expected_flag = FlagSnapshot {
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
    fn test_list_flags_in_package() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);

        let context_one = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        storage_files.stage_server_override(&context_one, "false").unwrap();
        let context_two = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "disabled_rw")
            .unwrap();
        storage_files.stage_server_override(&context_two, "false").unwrap();
        storage_files.stage_local_override(&context_two, "true").unwrap();
        storage_files.apply_all_staged_overrides().unwrap();

        let flags =
            storage_files.list_flags_in_package("com.android.aconfig.storage.test_1").unwrap();

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
    fn test_list_all_flags() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut storage_files = create_mock_storage_files(&container, &root_dir);

        let context_one = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_1", "enabled_rw")
            .unwrap();
        storage_files.stage_server_override(&context_one, "false").unwrap();
        let context_two = storage_files
            .get_package_flag_context("com.android.aconfig.storage.test_2", "disabled_rw")
            .unwrap();
        storage_files.stage_server_override(&context_two, "false").unwrap();
        storage_files.stage_local_override(&context_two, "true").unwrap();
        storage_files.apply_all_staged_overrides().unwrap();

        let flags = storage_files.list_all_flags().unwrap();
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
            package: String::from("com.android.aconfig.storage.test_2"),
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
        assert_eq!(flags[3], flag);
    }
}
