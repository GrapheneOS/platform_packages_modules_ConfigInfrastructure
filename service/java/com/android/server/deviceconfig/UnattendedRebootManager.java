package com.android.server.deviceconfig;

import android.annotation.NonNull;
import android.annotation.Nullable;
import android.app.AlarmManager;
import android.app.KeyguardManager;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.IntentSender;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.os.PowerManager;
import android.os.RecoverySystem;
import android.util.Log;
import com.android.internal.annotations.VisibleForTesting;
import java.io.IOException;
import java.time.Instant;
import java.time.LocalDateTime;
import java.time.ZoneId;

/**
 * Reboot scheduler for applying aconfig flags.
 *
 * <p>If device is password protected, uses <a
 * href="https://source.android.com/docs/core/ota/resume-on-reboot">Resume on Reboot</a> to reboot
 * the device, otherwise proceeds with regular reboot.
 *
 * @hide
 */
final class UnattendedRebootManager {
  private static final int DEFAULT_REBOOT_WINDOW_START_TIME_HOUR = 1;
  private static final int DEFAULT_REBOOT_WINDOW_END_TIME_HOUR = 5;

  private static final int DEFAULT_REBOOT_FREQUENCY_DAYS = 2;

  private static final String TAG = "UnattendedRebootManager";

  static final String REBOOT_REASON = "unattended,flaginfra";

  @VisibleForTesting
  static final String ACTION_RESUME_ON_REBOOT_LSKF_CAPTURED =
      "com.android.server.deviceconfig.RESUME_ON_REBOOOT_LSKF_CAPTURED";

  @VisibleForTesting
  static final String ACTION_TRIGGER_REBOOT = "com.android.server.deviceconfig.TRIGGER_REBOOT";

  private final Context mContext;

  private boolean mLskfCaptured;

  private final UnattendedRebootManagerInjector mInjector;

  private static class InjectorImpl implements UnattendedRebootManagerInjector {
    InjectorImpl() {
      /*no op*/
    }

    public long now() {
      return System.currentTimeMillis();
    }

    public ZoneId zoneId() {
      return ZoneId.systemDefault();
    }

    public int getRebootStartTime() {
      return DEFAULT_REBOOT_WINDOW_START_TIME_HOUR;
    }

    public int getRebootEndTime() {
      return DEFAULT_REBOOT_WINDOW_END_TIME_HOUR;
    }

    public int getRebootFrequency() {
      return DEFAULT_REBOOT_FREQUENCY_DAYS;
    }

    public void setRebootAlarm(Context context, long rebootTimeMillis) {
      AlarmManager alarmManager = context.getSystemService(AlarmManager.class);
      alarmManager.setExact(
          AlarmManager.RTC_WAKEUP, rebootTimeMillis, createTriggerRebootPendingIntent(context));
    }

    public void triggerRebootOnNetworkAvailable(Context context) {
      final ConnectivityManager connectivityManager =
          context.getSystemService(ConnectivityManager.class);
      NetworkRequest request =
          new NetworkRequest.Builder()
              .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
              .build();
      connectivityManager.requestNetwork(request, createTriggerRebootPendingIntent(context));
    }

    public int rebootAndApply(@NonNull Context context, @NonNull String reason, boolean slotSwitch)
        throws IOException {
      return RecoverySystem.rebootAndApply(context, reason, slotSwitch);
    }

    public void prepareForUnattendedUpdate(
        @NonNull Context context, @NonNull String updateToken, @Nullable IntentSender intentSender)
        throws IOException {
      RecoverySystem.prepareForUnattendedUpdate(context, updateToken, intentSender);
    }

    public boolean isPreparedForUnattendedUpdate(@NonNull Context context) throws IOException {
      return RecoverySystem.isPreparedForUnattendedUpdate(context);
    }

    public void regularReboot(Context context) {
      PowerManager powerManager = context.getSystemService(PowerManager.class);
      powerManager.reboot(REBOOT_REASON);
    }

    private static PendingIntent createTriggerRebootPendingIntent(Context context) {
      return PendingIntent.getBroadcast(
          context,
          /* requestCode= */ 0,
          new Intent(ACTION_TRIGGER_REBOOT),
          PendingIntent.FLAG_ONE_SHOT | PendingIntent.FLAG_IMMUTABLE);
    }
  }

  @VisibleForTesting
  UnattendedRebootManager(Context context, UnattendedRebootManagerInjector injector) {
    mContext = context;
    mInjector = injector;

    mContext.registerReceiver(
        new BroadcastReceiver() {
          @Override
          public void onReceive(Context context, Intent intent) {
            mLskfCaptured = true;
          }
        },
        new IntentFilter(ACTION_RESUME_ON_REBOOT_LSKF_CAPTURED),
        Context.RECEIVER_EXPORTED);

    // Do not export receiver so that tests don't trigger reboot.
    mContext.registerReceiver(
        new BroadcastReceiver() {
          @Override
          public void onReceive(Context context, Intent intent) {
            tryRebootOrSchedule();
          }
        },
        new IntentFilter(ACTION_TRIGGER_REBOOT),
        Context.RECEIVER_NOT_EXPORTED);
  }

  UnattendedRebootManager(Context context) {
    this(context, new InjectorImpl());
  }

  public void prepareUnattendedReboot() {
    Log.i(TAG, "Preparing for Unattended Reboot");
    // RoR only supported on devices with screen lock.
    if (!isDeviceSecure(mContext)) {
      return;
    }
    PendingIntent pendingIntent =
        PendingIntent.getBroadcast(
            mContext,
            /* requestCode= */ 0,
            new Intent(ACTION_RESUME_ON_REBOOT_LSKF_CAPTURED),
            PendingIntent.FLAG_ONE_SHOT | PendingIntent.FLAG_IMMUTABLE);

    try {
      mInjector.prepareForUnattendedUpdate(
          mContext, /* updateToken= */ "", pendingIntent.getIntentSender());
    } catch (IOException e) {
      Log.i(TAG, "prepareForUnattendedReboot failed with exception" + e.getLocalizedMessage());
    }
  }

  public void scheduleReboot() {
    // Reboot the next day at the reboot start time.
    LocalDateTime timeToReboot =
        Instant.ofEpochMilli(mInjector.now())
            .atZone(mInjector.zoneId())
            .toLocalDate()
            .plusDays(mInjector.getRebootFrequency())
            .atTime(mInjector.getRebootStartTime(), /* minute= */ 0);
    long rebootTimeMillis = timeToReboot.atZone(mInjector.zoneId()).toInstant().toEpochMilli();
    Log.v(TAG, "Scheduling unattended reboot at time " + timeToReboot);

    if (timeToReboot.isBefore(
        LocalDateTime.ofInstant(Instant.ofEpochMilli(mInjector.now()), mInjector.zoneId()))) {
      Log.w(TAG, "Reboot time has already passed.");
      return;
    }

    mInjector.setRebootAlarm(mContext, rebootTimeMillis);
  }

  @VisibleForTesting
  void tryRebootOrSchedule() {
    Log.v(TAG, "Attempting unattended reboot");

    // Is RoR is supported?
    if (!isDeviceSecure(mContext)) {
      Log.v(TAG, "Device is not secure. Proceed with regular reboot");
      mInjector.regularReboot(mContext);
      return;
    }
    // Is RoR prepared?
    if (!isPreparedForUnattendedReboot()) {
      Log.v(TAG, "Lskf is not captured, reschedule reboot.");
      prepareUnattendedReboot();
      scheduleReboot();
      return;
    }
    // Is network connected?
    // TODO(b/305259443): Use after-boot network connectivity projection
    if (!isNetworkConnected(mContext)) {
      Log.i(TAG, "Network is not connected, schedule reboot for another time.");
      mInjector.triggerRebootOnNetworkAvailable(mContext);
      return;
    }
    // Is current time between reboot window?
    int currentHour =
        Instant.ofEpochMilli(mInjector.now())
            .atZone(mInjector.zoneId())
            .toLocalDateTime()
            .getHour();
    if (currentHour < mInjector.getRebootStartTime()
        && currentHour >= mInjector.getRebootEndTime()) {
      Log.v(TAG, "Reboot requested outside of reboot window, reschedule.");
      prepareUnattendedReboot();
      scheduleReboot();
      return;
    }

    // Proceed with RoR.
    try {
      int success = mInjector.rebootAndApply(mContext, REBOOT_REASON, /* slotSwitch= */ false);
      if (success != 0) {
        // If reboot is not successful, reschedule.
        Log.w(TAG, "Unattended reboot failed, reschedule.");
        scheduleReboot();
      }
    } catch (IOException e) {
      Log.e(TAG, e.getLocalizedMessage());
      scheduleReboot();
    }
  }

  private boolean isPreparedForUnattendedReboot() {
    try {
      boolean isPrepared = mInjector.isPreparedForUnattendedUpdate(mContext);
      if (isPrepared != mLskfCaptured) {
        Log.w(TAG, "isPrepared != mLskfCaptured. Received " + isPrepared);
      }
      return isPrepared;
    } catch (IOException e) {
      Log.w(TAG, e.getLocalizedMessage());
      return mLskfCaptured;
    }
  }

  /** Returns true if the device has screen lock. */
  private static boolean isDeviceSecure(Context context) {
    KeyguardManager keyguardManager = context.getSystemService(KeyguardManager.class);
    if (keyguardManager == null) {
      // Unknown if device is locked, proceed with RoR anyway.
      Log.w(TAG, "Keyguard manager is null, proceeding with RoR anyway.");
      return true;
    }
    return keyguardManager.isDeviceSecure();
  }

  private static boolean isNetworkConnected(Context context) {
    final ConnectivityManager connectivityManager =
        context.getSystemService(ConnectivityManager.class);
    if (connectivityManager == null) {
      Log.w(TAG, "ConnectivityManager is null");
      return false;
    }
    Network activeNetwork = connectivityManager.getActiveNetwork();
    NetworkCapabilities networkCapabilities =
        connectivityManager.getNetworkCapabilities(activeNetwork);
    return networkCapabilities != null
        && networkCapabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
        && networkCapabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED);
  }
}
