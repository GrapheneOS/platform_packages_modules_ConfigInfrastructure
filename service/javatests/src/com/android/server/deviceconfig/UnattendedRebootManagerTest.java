package com.android.server.deviceconfig;

import static androidx.test.platform.app.InstrumentationRegistry.getInstrumentation;
import static com.android.server.deviceconfig.UnattendedRebootManager.ACTION_RESUME_ON_REBOOT_LSKF_CAPTURED;
import static com.android.server.deviceconfig.UnattendedRebootManager.ACTION_TRIGGER_REBOOT;
import static com.google.common.truth.Truth.assertThat;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;
import static org.mockito.Mockito.mock;
import static org.mockito.Mockito.when;

import androidx.annotation.NonNull;
import androidx.annotation.Nullable;
import android.app.KeyguardManager;
import android.content.BroadcastReceiver;
import android.content.ContextWrapper;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.IntentSender;
import android.util.Log;

import androidx.test.filters.SmallTest;
import java.io.IOException;
import java.time.ZoneId;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

import org.junit.Before;
import org.junit.Test;

@SmallTest
public class UnattendedRebootManagerTest {

  private static final int REBOOT_FREQUENCY = 1;
  private static final int REBOOT_HOUR = 2;
  private static final long CURRENT_TIME = 1696452549304L; // 2023-10-04T13:49:09.304
  private static final long REBOOT_TIME = 1696496400000L; // 2023-10-05T02:00:00

  private Context mContext;

  private KeyguardManager mKeyguardManager;

  FakeInjector mFakeInjector;

  private UnattendedRebootManager mRebootManager;

  @Before
  public void setUp() throws Exception {
    mKeyguardManager = mock(KeyguardManager.class);

    mContext =
        new ContextWrapper(getInstrumentation().getTargetContext()) {
          @Override
          public Object getSystemService(String name) {
            if (name.equals(Context.KEYGUARD_SERVICE)) {
              return mKeyguardManager;
            }
            return super.getSystemService(name);
          }
        };

    mFakeInjector = new FakeInjector();
    mRebootManager = new UnattendedRebootManager(mContext, mFakeInjector);

    // Need to register receiver in tests so that the test doesn't trigger reboot requested by
    // deviceconfig.
    mContext.registerReceiver(
        new BroadcastReceiver() {
          @Override
          public void onReceive(Context context, Intent intent) {
            mRebootManager.tryRebootOrSchedule();
          }
        },
        new IntentFilter(ACTION_TRIGGER_REBOOT),
        Context.RECEIVER_EXPORTED);
  }

  @Test
  public void scheduleReboot() {
    when(mKeyguardManager.isDeviceSecure()).thenReturn(true);

    mRebootManager.prepareUnattendedReboot();
    mRebootManager.scheduleReboot();

    assertThat(mFakeInjector.getActualRebootTime()).isEqualTo(REBOOT_TIME);
    assertTrue(mFakeInjector.isRebootAndApplied());
    assertFalse(mFakeInjector.isRegularRebooted());
  }

  @Test
  public void scheduleReboot_noPinLock() {
    when(mKeyguardManager.isDeviceSecure()).thenReturn(false);

    mRebootManager.prepareUnattendedReboot();
    mRebootManager.scheduleReboot();

    assertThat(mFakeInjector.getActualRebootTime()).isEqualTo(REBOOT_TIME);
    assertFalse(mFakeInjector.isRebootAndApplied());
    assertTrue(mFakeInjector.isRegularRebooted());
  }

  @Test
  public void scheduleReboot_noPreparation() {
    when(mKeyguardManager.isDeviceSecure()).thenReturn(true);

    mRebootManager.scheduleReboot();

    assertThat(mFakeInjector.getActualRebootTime()).isEqualTo(REBOOT_TIME);
    assertFalse(mFakeInjector.isRebootAndApplied());
    assertFalse(mFakeInjector.isRegularRebooted());
  }

  static class FakeInjector implements UnattendedRebootManagerInjector {

    private boolean isPreparedForUnattendedReboot;
    private boolean rebootAndApplied;
    private boolean regularRebooted;
    private long actualRebootTime;

    FakeInjector() {}

    @Override
    public void prepareForUnattendedUpdate(
        @NonNull Context context, @NonNull String updateToken, @Nullable IntentSender intentSender)
        throws IOException {
      context.sendBroadcast(new Intent(ACTION_RESUME_ON_REBOOT_LSKF_CAPTURED));
      isPreparedForUnattendedReboot = true;
    }

    @Override
    public boolean isPreparedForUnattendedUpdate(@NonNull Context context) throws IOException {
      return isPreparedForUnattendedReboot;
    }

    @Override
    public int rebootAndApply(
        @NonNull Context context, @NonNull String reason, boolean slotSwitch) {
      Log.i("UnattendedRebootManagerTest", "MockInjector.rebootAndApply");
      rebootAndApplied = true;
      return 0; // No error.
    }

    @Override
    public int getRebootFrequency() {
      return REBOOT_FREQUENCY;
    }

    @Override
    public void setRebootAlarm(Context context, long rebootTimeMillis) {
      // reboot immediately
      actualRebootTime = rebootTimeMillis;
      context.sendBroadcast(new Intent(UnattendedRebootManager.ACTION_TRIGGER_REBOOT));

      LatchingBroadcastReceiver rebootReceiver = new LatchingBroadcastReceiver();
      context.registerReceiver(
          rebootReceiver, new IntentFilter(ACTION_TRIGGER_REBOOT), Context.RECEIVER_EXPORTED);
      rebootReceiver.await(10, TimeUnit.SECONDS);
    }

    @Override
    public int getRebootStartTime() {
      return REBOOT_HOUR;
    }

    @Override
    public long now() {
      return CURRENT_TIME;
    }

    @Override
    public ZoneId zoneId() {
      return ZoneId.of("America/Los_Angeles");
    }

    @Override
    public void regularReboot(Context context) {
      Log.i("UnattendedRebootManagerTest", "MockInjector.regularRebooted");
      regularRebooted = true;
    }

    boolean isRebootAndApplied() {
      return rebootAndApplied;
    }

    boolean isRegularRebooted() {
      return regularRebooted;
    }

    public long getActualRebootTime() {
      return actualRebootTime;
    }
  }

  /**
   * A {@link BroadcastReceiver} with an internal latch that unblocks once any intent is received.
   */
  private static class LatchingBroadcastReceiver extends BroadcastReceiver {
    private CountDownLatch latch = new CountDownLatch(1);

    @Override
    public void onReceive(Context context, Intent intent) {
      latch.countDown();
    }

    public boolean await(long timeoutInMs, TimeUnit timeUnit) {
      try {
        return latch.await(timeoutInMs, timeUnit);
      } catch (InterruptedException e) {
        return false;
      }
    }
  }
}
