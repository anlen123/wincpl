package com.jianzang.phone

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.ComponentName
import android.content.Context
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.widget.EditText
import android.widget.TextView
import android.widget.Toast
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import com.google.android.material.button.MaterialButton
import com.google.android.material.materialswitch.MaterialSwitch
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions

class MainActivity : AppCompatActivity() {
    private lateinit var statusTitle: TextView
    private lateinit var statusDetail: TextView
    private lateinit var logText: TextView
    private lateinit var diagnosticText: TextView
    private lateinit var enableSwitch: MaterialSwitch

    private val preferenceListener =
        SharedPreferences.OnSharedPreferenceChangeListener { _, _ ->
            runOnUiThread { refresh() }
        }

    /** refresh() 里程序性修改开关状态时会触发监听器，用它挡掉。 */
    private var updating = false

    private val scanLauncher =
        registerForActivityResult(ScanContract()) { result ->
            val contents = result.contents
            if (contents.isNullOrBlank()) return@registerForActivityResult
            val pairing = PairingStore.parse(contents)
            if (pairing == null) {
                toast(getString(R.string.toast_bad_qr))
                return@registerForActivityResult
            }
            savePairing(pairing)
        }

    private val smsPermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            PairingStore.appendLog(this, if (granted) "接收短信权限：已授予" else "接收短信权限：被拒绝，请到系统应用权限中允许短信")
            if (granted) {
                applyEnabled(true)
            } else {
                applyEnabled(false)
                toast(getString(R.string.toast_no_permission))
            }
        }

    private val notificationPermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            PairingStore.appendLog(this, if (granted) "通知权限：已授予" else "通知权限：未授予（不影响短信转发，可查看最近记录）")
            refresh()
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        statusTitle = findViewById(R.id.statusTitle)
        statusDetail = findViewById(R.id.statusDetail)
        logText = findViewById(R.id.logText)
        diagnosticText = findViewById(R.id.diagnosticText)
        enableSwitch = findViewById(R.id.enableSwitch)

        findViewById<MaterialButton>(R.id.scanButton).setOnClickListener { startScan() }
        findViewById<MaterialButton>(R.id.manualButton).setOnClickListener { askForLink() }
        findViewById<MaterialButton>(R.id.testButton).setOnClickListener { testConnection() }
        findViewById<MaterialButton>(R.id.diagnoseButton).setOnClickListener {
            PairingStore.appendLog(this, "手动检查：${diagnosticSummary()}")
            refresh()
            if (!hasSmsPermission() && PairingStore.load(this) != null) requestEnable()
        }
        findViewById<MaterialButton>(R.id.copyLogsButton).setOnClickListener {
            val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
            clipboard.setPrimaryClip(
                ClipData.newPlainText("剪藏诊断日志", diagnosticSummary() + "\n" + PairingStore.log(this).joinToString("\n")),
            )
            toast(getString(R.string.logs_copied))
        }

        enableSwitch.setOnCheckedChangeListener { _, checked ->
            if (updating) return@setOnCheckedChangeListener
            if (checked) requestEnable() else applyEnabled(false)
        }
    }

    override fun onStart() {
        super.onStart()
        PairingStore.observe(this, preferenceListener)
    }

    override fun onStop() {
        PairingStore.unobserve(this, preferenceListener)
        super.onStop()
    }

    override fun onResume() {
        super.onResume()
        PairingStore.appendLog(this, "打开诊断版 ${BuildConfig.VERSION_NAME}：${diagnosticSummary()}")
        refresh()
    }

    private fun hasSmsPermission(): Boolean =
        ContextCompat.checkSelfPermission(this, Manifest.permission.RECEIVE_SMS) == PackageManager.PERMISSION_GRANTED

    private fun diagnosticSummary(): String {
        val enabled = PairingStore.isEnabled(this)
        val componentState = packageManager.getComponentEnabledSetting(ComponentName(this, SmsReceiver::class.java))
        val receiverEnabled =
            componentState == PackageManager.COMPONENT_ENABLED_STATE_DEFAULT ||
                componentState == PackageManager.COMPONENT_ENABLED_STATE_ENABLED
        val power = getSystemService(Context.POWER_SERVICE) as PowerManager
        return "转发=${if (enabled) "开启" else "关闭"}；短信权限=${if (hasSmsPermission()) "允许" else "未允许"}；" +
            "接收组件=${if (receiverEnabled) "启用" else "禁用"}；通知=${if (NotificationManagerCompat
                    .from(
                        this,
                    ).areNotificationsEnabled()
            ) {
                "允许"
            } else {
                "关闭"
            }}；" +
            "电池优化=${if (power.isIgnoringBatteryOptimizations(packageName)) "不受限制" else "受优化"}"
    }

    // ---------- 配对 ----------

    private fun startScan() {
        val options =
            ScanOptions()
                .setDesiredBarcodeFormats(ScanOptions.QR_CODE)
                .setPrompt(getString(R.string.scan))
                .setBeepEnabled(false)
                .setOrientationLocked(false)
        scanLauncher.launch(options)
    }

    private fun askForLink() {
        val input =
            EditText(this).apply {
                hint = getString(R.string.manual_hint)
                setSingleLine(true)
                val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                val pasted =
                    clipboard.primaryClip
                        ?.getItemAt(0)
                        ?.coerceToText(this@MainActivity)
                        ?.toString()
                if (!pasted.isNullOrBlank() && PairingStore.parse(pasted) != null) setText(pasted.trim())
            }
        AlertDialog
            .Builder(this)
            .setTitle(R.string.manual_title)
            .setView(input)
            .setNegativeButton(R.string.cancel, null)
            .setPositiveButton(R.string.ok) { _, _ ->
                val pairing = PairingStore.parse(input.text.toString())
                if (pairing == null) toast(getString(R.string.toast_bad_qr)) else savePairing(pairing)
            }.show()
    }

    private fun savePairing(pairing: Pairing) {
        PairingStore.save(this, pairing)
        PairingStore.appendLog(this, "已配对 ${pairing.address}")
        toast(getString(R.string.toast_paired, pairing.address))
        refresh()
        // 配对成功后直接把开关打开；权限不足时监听器会再退回关闭状态。
        updating = true
        enableSwitch.isChecked = true
        updating = false
        requestEnable()
    }

    // ---------- 开关与权限 ----------

    private fun requestEnable() {
        if (PairingStore.load(this) == null) {
            updating = true
            enableSwitch.isChecked = false
            updating = false
            toast(getString(R.string.toast_no_pairing))
            return
        }
        // 先拿短信权限，再请求通知，避免同时启动两个权限弹窗。
        val granted = hasSmsPermission()
        if (granted) {
            applyEnabled(true)
        } else {
            PairingStore.appendLog(this, "正在请求接收短信权限")
            smsPermission.launch(Manifest.permission.RECEIVE_SMS)
        }
    }

    private fun applyEnabled(enabled: Boolean) {
        PairingStore.setEnabled(this, enabled)
        SmsReceiver.setEnabled(this, enabled)
        PairingStore.appendLog(this, if (enabled) "已开启短信转发" else "已关闭短信转发")
        updating = true
        enableSwitch.isChecked = enabled
        updating = false
        refresh()
        if (enabled && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    // ---------- 连接测试 ----------

    private fun testConnection() {
        val pairing = PairingStore.load(this)
        if (pairing == null) {
            toast(getString(R.string.toast_no_pairing))
            return
        }
        PairingStore.appendLog(this, "开始连接测试 GET /api/v1/ping → ${pairing.address}")
        toast(getString(R.string.toast_testing))
        Thread {
            val reply = Relay.ping(pairing)
            runOnUiThread {
                when (reply) {
                    is Reply.Ok -> PairingStore.appendLog(this, "连接正常 · ${pairing.address}")
                    is Reply.Failed -> PairingStore.appendLog(this, "连接失败：${reply.message}")
                }
                refresh()
            }
        }.start()
    }

    // ---------- 界面刷新 ----------

    private fun refresh() {
        val pairing = PairingStore.load(this)
        if (pairing == null) {
            statusTitle.text = getString(R.string.status_unpaired)
            statusDetail.text = getString(R.string.status_unpaired_hint)
        } else {
            statusTitle.text = getString(R.string.status_paired, pairing.name)
            val masked = if (pairing.token.length > 8) "…${pairing.token.takeLast(6)}" else pairing.token
            statusDetail.text = getString(R.string.status_paired_detail, pairing.address, masked)
        }
        val wanted = pairing != null && PairingStore.isEnabled(this)
        if (enableSwitch.isChecked != wanted) {
            updating = true
            enableSwitch.isChecked = wanted
            updating = false
        }
        diagnosticText.text = diagnosticSummary()
        val lines = PairingStore.log(this)
        logText.text = if (lines.isEmpty()) getString(R.string.log_empty) else lines.joinToString("\n")
    }

    private fun toast(message: String) {
        Toast.makeText(this, message, Toast.LENGTH_SHORT).show()
    }
}
