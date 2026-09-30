package com.jianzang.phone

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

/**
 * 短信到达 → 预过滤 → 转发给电脑。
 * 关闭开关时会把这个组件整体禁用，所以平时进程不会被短信唤醒。
 */
class SmsReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Telephony.Sms.Intents.SMS_RECEIVED_ACTION) return
        if (!PairingStore.isEnabled(context)) return

        val appContext = context.applicationContext
        val pairing = PairingStore.load(appContext)
        if (pairing == null) {
            PairingStore.appendLog(appContext, "还没配对，短信已忽略")
            return
        }

        val messages = Telephony.Sms.Intents.getMessagesFromIntent(intent)
        if (messages.isEmpty()) return

        val from = messages.first().let { it.displayOriginatingAddress ?: it.originatingAddress } ?: "未知号码"
        val text = messages.joinToString(separator = "") { it.messageBody.orEmpty() }
        if (!looksLikeVerification(text)) return

        // goAsync() 让系统给我们一点时间在后台把请求发完。
        val pending = goAsync()
        Thread {
            try {
                when (val reply = Relay.sendSms(pairing, text, from)) {
                    is Reply.Ok -> {
                        val code = reply.code.orEmpty()
                        PairingStore.appendLog(appContext, "$from → 已送达 ${code.ifEmpty { "(未识别出验证码)" }}")
                        notify(appContext, appContext.getString(R.string.notify_ok_title), appContext.getString(R.string.notify_ok_body, code))
                    }
                    is Reply.Failed -> {
                        PairingStore.appendLog(appContext, "$from → 失败：${reply.message}")
                        notify(appContext, appContext.getString(R.string.notify_fail_title), reply.message)
                    }
                }
            } catch (error: Throwable) {
                PairingStore.appendLog(appContext, "$from → 出错：${error.message}")
            } finally {
                pending.finish()
            }
        }.start()
    }

    private fun looksLikeVerification(text: String): Boolean =
        KEYWORDS.any { text.contains(it, ignoreCase = true) }

    private fun notify(context: Context, title: String, body: String) {
        val manager = NotificationManagerCompat.from(context)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                context.getString(R.string.notify_channel),
                NotificationManager.IMPORTANCE_DEFAULT
            ).apply { description = context.getString(R.string.notify_channel_desc) }
            manager.createNotificationChannel(channel)
        }
        val notification = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setAutoCancel(true)
            .build()
        // 没有通知权限时静默失败即可，转发本身已经完成。
        runCatching { manager.notify(System.currentTimeMillis().toInt(), notification) }
    }

    companion object {
        private const val CHANNEL_ID = "jianzang-code"
        private val KEYWORDS = listOf("验证码", "校验码", "动态码", "确认码", "口令", "code", "pin")

        fun setEnabled(context: Context, enabled: Boolean) {
            val state = if (enabled) {
                PackageManager.COMPONENT_ENABLED_STATE_ENABLED
            } else {
                PackageManager.COMPONENT_ENABLED_STATE_DISABLED
            }
            context.packageManager.setComponentEnabledSetting(
                ComponentName(context, SmsReceiver::class.java),
                state,
                PackageManager.DONT_KILL_APP
            )
        }
    }
}
