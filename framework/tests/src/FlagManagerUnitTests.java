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

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotEquals;
import static org.junit.Assert.assertTrue;

import android.os.flagging.AconfigPackage;
import android.os.flagging.FlagManager;
import android.platform.test.annotations.RequiresFlagsEnabled;
import android.platform.test.flag.junit.CheckFlagsRule;
import android.platform.test.flag.junit.DeviceFlagsValueProvider;
import android.provider.flags.Flags;

import androidx.test.InstrumentationRegistry;

import org.junit.Rule;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.junit.runners.JUnit4;

import java.io.IOException;
import java.util.HashMap;
import java.util.HashSet;

@RunWith(JUnit4.class)
public class FlagManagerUnitTests {
    @Rule
    public final CheckFlagsRule mCheckFlagsRule = DeviceFlagsValueProvider.createCheckFlagsRule();

    @Test
    @RequiresFlagsEnabled(Flags.FLAG_NEW_STORAGE_PUBLIC_API)
    public void testSetBooleanLocalOverrideImmediately() throws IOException {
        FlagManager flagManager =
                InstrumentationRegistry.getInstrumentation()
                        .getContext()
                        .getSystemService(FlagManager.class);
        assertNotEquals(flagManager, null);

        HashMap<String, Boolean> flagsToValues = new HashMap();
        flagsToValues.put("android.provider.flags.flag_manager_unit_test_flag", true);
        flagManager.setBooleanLocalOverridesImmediately(flagsToValues);

        AconfigPackage aconfigPackage = AconfigPackage.load("android.provider.flags");
        boolean value = aconfigPackage.getBooleanFlagValue("flag_manager_unit_test_flag", false);

        assertTrue(value);

        HashSet<String> flagNames = new HashSet();
        flagNames.add("android.provider.flags.flag_manager_unit_test_flag");
        flagManager.clearBooleanLocalOverridesImmediately(flagNames);

        AconfigPackage aconfigPackageAfterClearing = AconfigPackage.load("android.provider.flags");
        boolean valueAfterClearing =
                aconfigPackageAfterClearing.getBooleanFlagValue(
                        "flag_manager_unit_test_flag", false);

        assertFalse(valueAfterClearing);
    }
}
