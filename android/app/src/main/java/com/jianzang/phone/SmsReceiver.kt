package com.jianzang.phone

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.provider.Telephony
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat

/**
 * 短信到达 → 预过滤 → 转发给电脑。
 * 关闭开关时会把这个组件整体禁用，所以平时进程不会被短信唤醒。
 */
class SmsReceiver : BroadcastReceiver() {
    override fun onReceive(
        context: Context,
        intent: Intent,
    ) {
        if (intent.action != Telephony.Sms.Intents.SMS_RECEIVED_ACTION) return
        val appContext = context.applicationContext
        PairingStore.appendLog(appContext, "收到短信广播 SMS_RECEIVED")
        if (!PairingStore.isEnabled(appContext)) {
            PairingStore.appendLog(appContext, "忽略短信：转发开关已关闭")
            return
        }
        val pairing = PairingStore.load(appContext)
        if (pairing == null) {
            PairingStore.appendLog(appContext, "还没配对，短信已忽略")
            return
        }

        val messages =
            runCatching { Telephony.Sms.Intents.getMessagesFromIntent(intent) }.getOrElse {
                PairingStore.appendLog(appContext, "解析短信广播失败：${it.javaClass.simpleName}")
                return
            }
        if (messages.isEmpty()) {
            PairingStore.appendLog(appContext, "忽略短信：广播里没有可解析的短信分段")
            return
        }

        val from = messages.first().let { it.displayOriginatingAddress ?: it.originatingAddress } ?: "未知号码"
        val text = messages.joinToString(separator = "") { it.messageBody.orEmpty() }
        PairingStore.appendLog(appContext, "短信解析完成：${messages.size} 个分段，${text.length} 字符")
        if (!VerificationFilter.matches(text)) {
            PairingStore.appendLog(appContext, "忽略短信：未匹配验证码关键词（不记录正文）")
            return
        }
        PairingStore.appendLog(appContext, "匹配验证码关键词，开始发送到 ${pairing.address}")

        // goAsync() 让系统给我们一点时间在后台把请求发完。
        val pending = goAsync()
        Thread {
            try {
                when (val reply = Relay.sendSms(pairing, text, from)) {
                    is Reply.Ok -> {
                        val code = reply.code.orEmpty()
                        PairingStore.appendLog(
                            appContext,
                            "短信接口返回成功：${if (code.isEmpty()) "未返回验证码" else "已识别 ${code.length} 位验证码"}（请核对电脑剪贴板）",
                        )
                        notify(
                            appContext,
                            appContext.getString(R.string.notify_ok_title),
                            appContext.getString(R.string.notify_ok_body, code),
                        )
                    }

                    is Reply.Failed -> {
                        PairingStore.appendLog(appContext, "短信发送失败：${reply.message}")
                        notify(appContext, appContext.getString(R.string.notify_fail_title), reply.message)
                    }
                }
            } catch (error: Throwable) {
                PairingStore.appendLog(appContext, "短信转发异常：${error.javaClass.simpleName}")
            } finally {
                pending.finish()
            }
        }.start()
    }

    private fun notify(
        context: Context,
        title: String,
        body: String,
    ) {
        val manager = NotificationManagerCompat.from(context)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel =
                NotificationChannel(
                    CHANNEL_ID,
                    context.getString(R.string.notify_channel),
                    NotificationManager.IMPORTANCE_DEFAULT,
                ).apply { description = context.getString(R.string.notify_channel_desc) }
            manager.createNotificationChannel(channel)
        }
        val notification =
            NotificationCompat
                .Builder(context, CHANNEL_ID)
                .setSmallIcon(android.R.drawable.ic_dialog_email)
                .setContentTitle(title)
                .setContentText(body)
                .setStyle(NotificationCompat.BigTextStyle().bigText(body))
                .setAutoCancel(true)
                .build()
        // 没有通知权限时静默失败即可，转发本身已经完成。
        if ((
                Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
                    ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
            ) ||
            !manager.areNotificationsEnabled()
        ) {
            PairingStore.appendLog(context, "系统通知已关闭：转发结果仍可在最近记录中查看")
            return
        }
        runCatching { manager.notify(System.currentTimeMillis().toInt(), notification) }
            .onFailure { PairingStore.appendLog(context, "显示通知失败：${it.javaClass.simpleName}") }
    }

    companion object {
        private const val CHANNEL_ID = "jianzang-code"

        fun setEnabled(
            context: Context,
            enabled: Boolean,
        ) {
            val state =
                if (enabled) {
                    PackageManager.COMPONENT_ENABLED_STATE_ENABLED
                } else {
                    PackageManager.COMPONENT_ENABLED_STATE_DISABLED
                }
            context.packageManager.setComponentEnabledSetting(
                ComponentName(context, SmsReceiver::class.java),
                state,
                PackageManager.DONT_KILL_APP,
            )
        }
    }
}
