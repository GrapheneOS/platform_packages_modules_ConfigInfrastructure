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
import static org.junit.Assert.assertNotEquals;
import static org.junit.Assert.assertThrows;

import org.junit.Test;
import org.junit.runner.RunWith;
import org.junit.runners.JUnit4;
import org.junit.Rule;

import android.os.flagging.AconfigStorageWriteException;
import android.os.flagging.FlagManager;
import android.platform.test.annotations.RequiresFlagsEnabled;
import android.platform.test.flag.junit.CheckFlagsRule;
import android.platform.test.flag.junit.DeviceFlagsValueProvider;
import android.provider.flags.Flags;
import androidx.test.InstrumentationRegistry;

import java.io.IOException;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;

@RunWith(JUnit4.class)
public class AconfigPublicApiCtsTests {
    @Rule
    public final CheckFlagsRule mCheckFlagsRule =
            DeviceFlagsValueProvider.createCheckFlagsRule();

    @Test
    @android.platform.test.annotations.DisabledOnRavenwood(blockedBy = FlagManager.class)
    @RequiresFlagsEnabled(Flags.FLAG_NEW_STORAGE_PUBLIC_API)
    public void testTestProcessCannotCallWriteApis() throws IOException {
        FlagManager flagManager =
                InstrumentationRegistry.getInstrumentation()
                        .getContext()
                        .getSystemService(FlagManager.class);
        assertNotEquals(flagManager, null);

        assertThrows(
                AconfigStorageWriteException.class,
                () ->
                        flagManager.setBooleanOverridesOnSystemBuildFingerprint(
                                "test_fingerprint", new HashMap()));

        assertThrows(
                AconfigStorageWriteException.class,
                () -> flagManager.setBooleanOverridesOnReboot(new HashMap()));

        assertThrows(
                AconfigStorageWriteException.class,
                () -> flagManager.setBooleanLocalOverridesOnReboot(new HashMap()));

        assertThrows(
                AconfigStorageWriteException.class,
                () -> flagManager.setBooleanLocalOverridesImmediately(new HashMap()));

        assertThrows(
                AconfigStorageWriteException.class,
                () -> flagManager.clearBooleanLocalOverridesImmediately(new HashSet()));

        assertThrows(
                AconfigStorageWriteException.class,
                () -> flagManager.clearBooleanLocalOverridesOnReboot(new HashSet()));
    }

    @Test
    @RequiresFlagsEnabled(Flags.FLAG_NEW_STORAGE_PUBLIC_API)
    public void testAconfigStorageWriteException(){
        // create new instance of AconfigStorageWriteException
        AconfigStorageWriteException exception =
            new AconfigStorageWriteException("test message");
        assertEquals(exception.getMessage(), "test message");

        Exception cause = new Exception("test cause");
        exception = new AconfigStorageWriteException("test message", cause);
        assertEquals(exception.getMessage(), "test message");
        assertEquals(exception.getCause(), cause);
    }
}
