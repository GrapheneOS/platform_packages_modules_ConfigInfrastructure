package com.android.server.deviceconfig;

import java.io.FileDescriptor;

import android.annotation.NonNull;
import android.annotation.SystemApi;
import android.content.Context;
import android.os.Binder;
import android.provider.DeviceConfig;
import android.provider.DeviceConfigManager;
import android.provider.UpdatableDeviceConfigServiceReadiness;

import android.provider.aidl.IDeviceConfigManager;

import com.android.server.SystemService;

/** @hide */
@SystemApi(client = SystemApi.Client.SYSTEM_SERVER)
public class DeviceConfigInit {

    private DeviceConfigInit() {
        // do not instantiate
    }

    /** @hide */
    @SystemApi(client = SystemApi.Client.SYSTEM_SERVER)
    public static class Lifecycle extends SystemService {
        private DeviceConfigServiceImpl mService;

        /** @hide */
        @SystemApi(client = SystemApi.Client.SYSTEM_SERVER)
        public Lifecycle(@NonNull Context context) {
            super(context);
            // this service is always instantiated but should only launch subsequent service(s)
            // if the module is ready
            if (UpdatableDeviceConfigServiceReadiness.shouldStartUpdatableService()) {
                mService = new DeviceConfigServiceImpl(getContext());
                publishBinderService(DeviceConfig.SERVICE_NAME, mService);
            }
        }

        /**
         * @hide
         */
        @Override
        public void onStart() {
            // no op
        }

        /**
         * Apply staged flags on boot.
         *
         * @param phase one of {@code SystemService.BootPhase}
         * @hide
         */
        @Override
        public void onBootPhase(/* @BootPhase */ int phase) {
            // TODO(b/286057899): move this earlier in the boot process
            if (phase == SystemService.PHASE_BOOT_COMPLETED) {
                DeviceConfigApplyStagedFlags.applyStagedFlags();
            }
        }
    }
}
