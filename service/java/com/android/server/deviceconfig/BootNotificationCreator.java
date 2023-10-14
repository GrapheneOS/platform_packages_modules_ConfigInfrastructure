package com.android.server.deviceconfig;

import android.annotation.NonNull;
import android.app.AlarmManager;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.IntentFilter;
import android.content.BroadcastReceiver;
import android.content.Intent;
import android.content.pm.PackageManager.NameNotFoundException;
import android.content.Context;
import android.graphics.drawable.Icon;
import android.os.PowerManager;
import android.provider.DeviceConfig.OnPropertiesChangedListener;
import android.provider.DeviceConfig.Properties;
import android.util.Slog;
import com.android.server.deviceconfig.resources.R;
import java.time.Instant;
import java.time.LocalDateTime;
import java.time.ZoneId;
import java.time.ZonedDateTime;

import static android.app.NotificationManager.IMPORTANCE_HIGH;

/**
 * Creates notifications when flags are staged on the device.
 *
 * The notification alerts the user to reboot, to apply the staged flags.
 *
 * @hide
 */
class BootNotificationCreator implements OnPropertiesChangedListener {
    private static final String TAG = "DeviceConfigBootNotificationCreator";

    private static final String RESOURCES_PACKAGE =
        "com.android.server.deviceconfig.resources";

    private static final String REBOOT_REASON = "DeviceConfig";

    private static final String ACTION_TRIGGER_HARD_REBOOT =
        "com.android.server.deviceconfig.TRIGGER_HARD_REBOOT";
    private static final String ACTION_POST_NOTIFICATION =
        "com.android.server.deviceconfig.POST_NOTIFICATION";

    private static final String CHANNEL_ID = "trunk-stable-flags";
    private static final String CHANNEL_NAME = "Trunkfood flags";
    private static final int NOTIFICATION_ID = 111555;

    private NotificationManager notificationManager;
    private PowerManager powerManager;
    private AlarmManager alarmManager;

    private Context context;

    private static final int REBOOT_HOUR = 18;
    private static final int REBOOT_MINUTE = 2;

    public BootNotificationCreator(@NonNull Context context) {
        this.context = context;

        this.context.registerReceiver(
            new HardRebootBroadcastReceiver(),
            new IntentFilter(ACTION_TRIGGER_HARD_REBOOT),
            Context.RECEIVER_EXPORTED);
        this.context.registerReceiver(
            new PostNotificationBroadcastReceiver(),
            new IntentFilter(ACTION_POST_NOTIFICATION),
            Context.RECEIVER_EXPORTED);
    }

    @Override
    public void onPropertiesChanged(Properties properties) {
        if (!tryInitializeDependenciesIfNeeded()) {
            Slog.i(TAG, "not posting notif; service dependencies not ready");
            return;
        }

        PendingIntent pendingIntent =
            PendingIntent.getBroadcast(
                context,
                /* requestCode= */ 1,
                new Intent(ACTION_POST_NOTIFICATION),
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);

        ZonedDateTime now = Instant
            .ofEpochMilli(System.currentTimeMillis())
            .atZone(ZoneId.systemDefault());

        LocalDateTime currentTime = now.toLocalDateTime();
        LocalDateTime postTime = now.toLocalDate().atTime(REBOOT_HOUR, REBOOT_MINUTE);

        LocalDateTime scheduledPostTime =
            currentTime.isBefore(postTime) ? postTime : postTime.plusDays(1);
        long scheduledPostTimeLong = scheduledPostTime
            .atZone(ZoneId.systemDefault())
            .toInstant()
            .toEpochMilli();

        alarmManager.setExact(
            AlarmManager.RTC_WAKEUP, scheduledPostTimeLong, pendingIntent);
    }

    private class PostNotificationBroadcastReceiver extends BroadcastReceiver {
        @Override
        public void onReceive(Context context, Intent intent) {
            PendingIntent pendingIntent =
                PendingIntent.getBroadcast(
                    context,
                    /* requestCode= */ 1,
                    new Intent(ACTION_TRIGGER_HARD_REBOOT),
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);

            try {
                Context resourcesContext = context.createPackageContext(RESOURCES_PACKAGE, 0);
                Notification notification = new Notification.Builder(context, CHANNEL_ID)
                    .setContentText(resourcesContext.getString(R.string.boot_notification_content))
                    .setContentTitle(resourcesContext.getString(R.string.boot_notification_title))
                    .setSmallIcon(Icon.createWithResource(resourcesContext, R.drawable.ic_flag))
                    .setContentIntent(pendingIntent)
                    .build();
                notificationManager.notify(NOTIFICATION_ID, notification);
            } catch (NameNotFoundException e) {
                Slog.e(TAG, "failed to post boot notification", e);
            }
        }
    }

    private class HardRebootBroadcastReceiver extends BroadcastReceiver {
        @Override
        public void onReceive(Context context, Intent intent) {
            powerManager.reboot(REBOOT_REASON);
        }
    }

    /**
     * If deps are not initialized yet, try to initialize them.
     *
     * @return true if the dependencies are newly or already initialized,
     *         or false if they are not ready yet
     */
    private boolean tryInitializeDependenciesIfNeeded() {
        if (notificationManager == null) {
            notificationManager = context.getSystemService(NotificationManager.class);
            if (notificationManager != null) {
                notificationManager.createNotificationChannel(
                    new NotificationChannel(CHANNEL_ID, CHANNEL_NAME, IMPORTANCE_HIGH));
            }
        }

        if (alarmManager == null) {
            alarmManager = context.getSystemService(AlarmManager.class);
        }

        if (powerManager == null) {
            powerManager = context.getSystemService(PowerManager.class);
        }

        return notificationManager != null
            && alarmManager != null
            && powerManager != null;
    }
}
