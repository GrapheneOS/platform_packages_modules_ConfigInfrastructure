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

package android.os.flagging.test;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertThrows;

import android.aconfig.DeviceProtosTestUtil;
import android.aconfig.nano.Aconfig;
import android.aconfig.nano.Aconfig.parsed_flag;
import android.aconfig.storage.FlagTable;
import android.aconfig.storage.FlagValueList;
import android.aconfig.storage.PackageTable;
import android.aconfig.storage.StorageFileProvider;
import android.os.flagging.AconfigPackage;
import android.os.flagging.AconfigStorageReadException;

import org.junit.Test;
import org.junit.runner.RunWith;
import org.junit.runners.JUnit4;

import java.io.IOException;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

@RunWith(JUnit4.class)
public class AconfigPackageTests {

    @Test
    public void testAconfigPackage_StorageFilesCache() throws IOException {
        List<parsed_flag> flags = DeviceProtosTestUtil.loadAndParseFlagProtos();
        for (parsed_flag flag : flags) {
            if (flag.permission == Aconfig.READ_ONLY && flag.state == Aconfig.DISABLED) {
                continue;
            }
            String container = flag.container;
            String packageName = flag.package_;
            assertNotNull(AconfigPackage.load(packageName));
        }
    }

    @Test
    public void testExternalAconfigPackageInstance() throws IOException {
        List<parsed_flag> flags = DeviceProtosTestUtil.loadAndParseFlagProtos();
        Map<String, AconfigPackage> readerMap = new HashMap<>();
        StorageFileProvider fp = StorageFileProvider.getDefaultProvider();

        for (parsed_flag flag : flags) {
            if (flag.permission == Aconfig.READ_ONLY && flag.state == Aconfig.DISABLED) {
                continue;
            }
            String container = flag.container;
            String packageName = flag.package_;
            String flagName = flag.name;

            PackageTable pTable = fp.getPackageTable(container);
            PackageTable.Node pNode = pTable.get(packageName);
            FlagTable fTable = fp.getFlagTable(container);
            FlagTable.Node fNode = fTable.get(pNode.getPackageId(), flagName);
            FlagValueList fList = fp.getFlagValueList(container);
            boolean rVal = fList.getBoolean(pNode.getBooleanStartIndex() + fNode.getFlagIndex());

            AconfigPackage reader = readerMap.get(packageName);
            if (reader == null) {
                reader = AconfigPackage.load(packageName);
                readerMap.put(packageName, reader);
            }
            // TODO: b/377311211 - Update to reflect redacted values once
            // implemented.
            boolean jVal = reader.getBooleanFlagValue(flagName, false);

            assertEquals(rVal, jVal);
        }
    }

    @Test
    public void testAconfigPackage_load_withError() {
        // load fake package
        AconfigStorageReadException e =
                assertThrows(
                        AconfigStorageReadException.class,
                        () -> AconfigPackage.load("fake_package"));
        assertEquals(AconfigStorageReadException.ERROR_PACKAGE_NOT_FOUND, e.getErrorCode());
    }
}
