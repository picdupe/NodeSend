package com.picdupe.nodesend

import android.app.Activity
import android.app.AlertDialog
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.database.Cursor
import android.net.Uri
import android.provider.OpenableColumns
import android.view.View
import android.view.ViewGroup
import android.widget.ArrayAdapter
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import androidx.activity.result.ActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.documentfile.provider.DocumentFile
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.io.FileOutputStream
import java.util.UUID
import java.util.concurrent.Executors

@InvokeArg
class PickArgs {
    var directory: Boolean = false
    var kind: String = "files"
}

@InvokeArg
class EventHandlerArgs {
    lateinit var handler: Channel
}

@InvokeArg
class ExportReceivedArgs {
    var root: String = ""
    var taskId: String = ""
    var targets: Array<String?> = emptyArray()
    var conflict: String = "rename"
}

@InvokeArg
class NotificationArgs { var payload: String = "[]" }

@TauriPlugin
class NodeSendImportPlugin(private val activity: Activity) : Plugin(activity) {
    private val pending = mutableListOf<String>()
    private var eventHandler: Channel? = null
    private var pickingDirectory = false
    private var pickingReceiveDirectory = false
    private var pickerBusy = false
    private val importWorker = Executors.newSingleThreadExecutor()
    private val notifiedTasks = mutableMapOf<String, String>()
    private var notificationWebView: android.webkit.WebView? = null

    private fun resolvePaths(invoke: Invoke, paths: List<String>) {
        invoke.resolve(JSObject().apply { put("paths", JSArray(paths)) })
    }

    private fun importPaths(invoke: Invoke, work: () -> List<String>) {
        importWorker.execute {
            runCatching(work)
                .onSuccess { resolvePaths(invoke, it) }
                .onFailure { invoke.reject("无法导入所选内容：${it.message ?: "读取失败"}") }
        }
    }

    @Command
    fun readClipboard(invoke: Invoke) {
        // Clipboard access must happen while our activity has focus, on the UI thread.
        activity.runOnUiThread {
            runCatching {
                val clipboard = activity.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                val clip = clipboard.primaryClip
                if (clip == null || clip.itemCount == 0) throw IllegalStateException("剪贴板为空，请先复制内容")
                val item = clip.getItemAt(0)
                val uri = item.uri
                val text = item.text?.toString()
                when {
                    uri != null -> importPaths(invoke) { listOf(copyUri(uri)) }
                    !text.isNullOrEmpty() -> importPaths(invoke) { listOf(writeSharedText(text)) }
                    else -> invoke.reject("当前剪贴板内容无法读取，请复制文本或使用分享功能导入文件。")
                }
            }.onFailure { invoke.reject("无法读取剪贴板：${it.message ?: "请重新复制后再试"}") }
        }
    }

    override fun load(webView: android.webkit.WebView) {
        super.load(webView)
        notificationWebView = webView
        receiveShareIntent(activity.intent)
    }

    override fun onNewIntent(intent: Intent) {
        receiveShareIntent(intent)
        if (intent.getBooleanExtra("nodesend-open-transfers", false)) {
            activity.intent = intent
        }
    }

    @Command
    fun updateTransferNotifications(invoke: Invoke) {
        val tasks = org.json.JSONArray(invoke.parseArgs(NotificationArgs::class.java).payload)
        activity.runOnUiThread {
            runCatching {
                if (activity.intent?.getBooleanExtra("nodesend-open-transfers", false) == true) {
                    notificationWebView?.evaluateJavascript("(function(){if(document.documentElement.dataset.nodeSendNavigationReady!=='true')return false;window.dispatchEvent(new Event('nodesend-open-transfers'));return true;})()") { ready ->
                        if (ready == "true") activity.intent?.removeExtra("nodesend-open-transfers")
                    }
                }
                val activeStates = setOf("queued", "connecting", "accepted", "waiting", "transferring", "paused", "interrupted")
                val hasActive = (0 until tasks.length()).any { tasks.getJSONObject(it).getString("status") in activeStates }
                if (android.os.Build.VERSION.SDK_INT >= 33 && activity.checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) != android.content.pm.PackageManager.PERMISSION_GRANTED) {
                    val preferences = activity.getSharedPreferences("notification-permission", Context.MODE_PRIVATE)
                    if (hasActive && activity.hasWindowFocus() && !preferences.getBoolean("requested", false)) {
                        preferences.edit().putBoolean("requested", true).apply()
                        androidx.core.app.ActivityCompat.requestPermissions(activity, arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 3215)
                    }
                    return@runCatching
                }
                val manager = activity.getSystemService(Context.NOTIFICATION_SERVICE) as android.app.NotificationManager
                val channel = "nodesend-transfers"
                if (android.os.Build.VERSION.SDK_INT >= 26) manager.createNotificationChannel(android.app.NotificationChannel(channel, "文件传输", android.app.NotificationManager.IMPORTANCE_LOW))
                val ids = mutableSetOf<String>()
                for (index in 0 until tasks.length()) {
                    val task = tasks.getJSONObject(index)
                    val id = task.getString("id")
                    ids.add(id)
                    val status = task.getString("status")
                    val active = status in activeStates
                    val bytes = task.getLong("completed_bytes")
                    val total = task.getLong("total_bytes")
                    val percent = if (total == 0L) 0 else ((bytes.toDouble() / total) * 100).toInt().coerceIn(0, 100)
                    val signature = "$status:$percent"
                    if (notifiedTasks[id] == signature || (!active && !notifiedTasks.containsKey(id))) continue
                    notifiedTasks[id] = signature
                    val description = when(status) { "completed" -> "已完成"; "failed" -> "传输失败"; "paused" -> "已暂停"; "cancelled" -> "已取消"; "rejected" -> "已拒绝"; "waiting" -> "等待确认"; "connecting" -> "正在连接"; "interrupted" -> "等待重试"; else -> "正在传输" }
                    val intent = Intent(activity, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP).putExtra("nodesend-open-transfers", true)
                    val pending = android.app.PendingIntent.getActivity(activity, 42, intent, android.app.PendingIntent.FLAG_UPDATE_CURRENT or android.app.PendingIntent.FLAG_IMMUTABLE)
                    val notification = androidx.core.app.NotificationCompat.Builder(activity, channel)
                        .setSmallIcon(android.R.drawable.stat_sys_upload_done)
                        .setContentTitle((if (task.getString("direction") == "send") "发送至 " else "接收自 ") + task.getString("peer_name"))
                        .setContentText("$description · ${formatBytes(bytes)} / ${formatBytes(total)}")
                        .setContentIntent(pending).setOnlyAlertOnce(true).setOngoing(active).setAutoCancel(!active)
                        .setCategory(androidx.core.app.NotificationCompat.CATEGORY_PROGRESS)
                        .setVisibility(androidx.core.app.NotificationCompat.VISIBILITY_PRIVATE)
                    if (active) notification.setProgress(100, percent, status in setOf("queued", "connecting", "waiting"))
                    manager.notify(id, 0, notification.build())
                }
                notifiedTasks.keys.filter { it !in ids }.toList().forEach { manager.cancel(it, 0); notifiedTasks.remove(it) }
            }.onSuccess { invoke.resolve() }.onFailure { invoke.reject(it.message ?: "无法更新传输通知") }
        }
    }

    @Command
    fun setEventHandler(invoke: Invoke) {
        eventHandler = invoke.parseArgs(EventHandlerArgs::class.java).handler
        notifySharedFiles(pending.toList())
        invoke.resolve()
    }

    @Command
    fun takePending(invoke: Invoke) {
        val result = JSArray()
        pending.toList().forEach(result::put)
        pending.clear()
        invoke.resolve(JSObject().apply { put("paths", result) })
    }

    @Command
    fun pick(invoke: Invoke) {
        val args = invoke.parseArgs(PickArgs::class.java)
        activity.runOnUiThread {
        if (pickerBusy) { invoke.reject("请先完成当前选择。"); return@runOnUiThread }
        pickerBusy = true
        if (args.kind == "apps") { pickApplication(invoke); return@runOnUiThread }
        pickingReceiveDirectory = args.directory && args.kind == "receive"
        pickingDirectory = args.directory && args.kind == "files"
        val intent = if (args.kind == "media") {
            // AndroidX selects the system photo picker, including its Albums tab.
            // On older devices without it, only the system media-document picker is used.
            ActivityResultContracts.PickMultipleVisualMedia().createIntent(
                activity, PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo)
            )
        } else if (args.directory) {
            Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).addFlags(readFlags() or Intent.FLAG_GRANT_PREFIX_URI_PERMISSION or
                (if (pickingReceiveDirectory) Intent.FLAG_GRANT_WRITE_URI_PERMISSION else 0))
        } else {
            Intent(Intent.ACTION_OPEN_DOCUMENT)
                .addCategory(Intent.CATEGORY_OPENABLE)
                .setType("*/*")
                .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
                .addFlags(readFlags())
        }
        runCatching { startActivityForResult(invoke, intent, "onPickResult") }
            .onFailure { pickerBusy = false; invoke.reject("无法打开选择器：${it.message}") }
        }
    }

    private data class AppChoice(val app: android.content.pm.ApplicationInfo, val name: String, val version: String, val bytes: Long, val launchable: Boolean)

    private fun pickApplication(invoke: Invoke) {
        importWorker.execute {
            runCatching {
                val manager = activity.packageManager
                @Suppress("DEPRECATION")
                val apps = manager.getInstalledApplications(0).map { app ->
                    val version = runCatching { manager.getPackageInfo(app.packageName, 0).versionName ?: "未知版本" }.getOrDefault("未知版本")
                    AppChoice(app, manager.getApplicationLabel(app).toString(), version,
                        (listOf(app.sourceDir) + (app.splitSourceDirs?.toList() ?: emptyList())).sumOf { File(it).length() },
                        manager.getLaunchIntentForPackage(app.packageName) != null)
                }.sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER) { it.name })
                activity.runOnUiThread { showApplicationPicker(invoke, apps) }
            }.onFailure { activity.runOnUiThread { pickerBusy = false; invoke.reject("无法读取应用列表：${it.message}") } }
        }
    }

    private fun showApplicationPicker(invoke: Invoke, apps: List<AppChoice>) {
        fun dp(value: Int) = (value * activity.resources.displayMetrics.density).toInt()
        val dark = activity.resources.configuration.uiMode and android.content.res.Configuration.UI_MODE_NIGHT_MASK == android.content.res.Configuration.UI_MODE_NIGHT_YES
        val foreground = if (dark) 0xffe0e5ef.toInt() else 0xff202b42.toInt()
        val muted = if (dark) 0xff9ba8bc.toInt() else 0xff68758d.toInt()
        val surface = if (dark) 0xff202939.toInt() else 0xfff8f9fc.toInt()
        fun rounded() = android.graphics.drawable.GradientDrawable().apply { setColor(surface); cornerRadius = dp(18).toFloat(); setStroke(dp(1), if (dark) 0xff344052.toInt() else 0xffd2d8e4.toInt()) }
        fun label(value: String, size: Float, color: Int) = TextView(activity).apply { text = value; textSize = size; setTextColor(color); maxLines = 1; ellipsize = android.text.TextUtils.TruncateAt.END }
        val dialog = android.app.Dialog(activity)
        val body = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL; background = rounded(); setPadding(dp(16), dp(12), dp(16), dp(12)) }
        val header = LinearLayout(activity).apply { gravity = android.view.Gravity.CENTER_VERTICAL }
        val back = label("‹", 32f, foreground).apply { gravity = android.view.Gravity.CENTER; contentDescription = "返回，关闭应用选择"; setOnClickListener { dialog.cancel() } }
        val menu = label("⋮", 26f, foreground).apply { gravity = android.view.Gravity.CENTER; contentDescription = "应用筛选" }
        header.addView(back, LinearLayout.LayoutParams(dp(44), dp(44)))
        header.addView(label("选择应用", 19f, foreground), LinearLayout.LayoutParams(0, dp(36), 1f))
        header.addView(menu, LinearLayout.LayoutParams(dp(44), dp(44)))
        body.addView(header)
        val search = android.widget.EditText(activity).apply { hint = "搜索应用名或包名"; textSize = 14f; setTextColor(foreground); setHintTextColor(muted); setSingleLine(); background = rounded(); setPadding(dp(12), 0, dp(12), 0) }
        body.addView(search, LinearLayout.LayoutParams(-1, dp(44)).apply { topMargin = dp(10) })
        val count = label("", 12f, muted).apply { setPadding(dp(2), dp(10), 0, dp(8)) }
        body.addView(count)
        var excludeSystem = true
        var excludeUnlaunchable = true
        val visible = mutableListOf<AppChoice>()
        var appRowHeight = dp(60)
        val adapter = object : ArrayAdapter<AppChoice>(activity, 0, visible) {
            override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
                val item = visible[position]
                val row = LinearLayout(activity).apply { gravity = android.view.Gravity.CENTER_VERTICAL; setPadding(dp(4), dp(3), dp(4), dp(3)); layoutParams = android.widget.AbsListView.LayoutParams(-1, appRowHeight) }
                val icon = ImageView(activity).apply { setImageDrawable(item.app.loadIcon(activity.packageManager)); scaleType = ImageView.ScaleType.FIT_CENTER; importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO }
                row.addView(icon, LinearLayout.LayoutParams(minOf(dp(44), appRowHeight - dp(8)), minOf(dp(44), appRowHeight - dp(8))).apply { rightMargin = dp(10) })
                val details = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL; gravity = android.view.Gravity.CENTER_VERTICAL }
                details.addView(label(item.name, 14f, foreground))
                details.addView(label("${formatBytes(item.bytes)}  ·  ${item.version}", 11f, muted).apply { setPadding(0, 0, 0, 0) })
                details.addView(label(item.app.packageName, 10f, muted))
                row.addView(details, LinearLayout.LayoutParams(0, -1, 1f))
                return row
            }
        }
        fun filter() {
            val query = search.text.toString().trim()
            visible.clear()
            visible.addAll(apps.filter {
                (!excludeSystem || it.app.flags and (android.content.pm.ApplicationInfo.FLAG_SYSTEM or android.content.pm.ApplicationInfo.FLAG_UPDATED_SYSTEM_APP) == 0) &&
                (!excludeUnlaunchable || it.launchable) && (it.name.contains(query, true) || it.app.packageName.contains(query, true))
            })
            count.text = "${visible.size} 个应用"
            adapter.notifyDataSetChanged()
        }
        val list = android.widget.ListView(activity).apply { divider = null; setSelector(android.graphics.drawable.ColorDrawable(android.graphics.Color.TRANSPARENT)); this.adapter = adapter }
        body.addView(list, LinearLayout.LayoutParams(-1, 0, 1f))
        list.addOnLayoutChangeListener { _, _, top, _, bottom, _, _, _, _ ->
            val height = minOf(dp(64), ((bottom - top) / 9)).coerceAtLeast(dp(48))
            if (height != appRowHeight) { appRowHeight = height; adapter.notifyDataSetChanged() }
        }
        search.addTextChangedListener(object : android.text.TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) { filter() }
            override fun afterTextChanged(s: android.text.Editable?) {}
        })
        var popup: android.widget.PopupWindow? = null
        menu.setOnClickListener {
            val options = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL; background = rounded(); setPadding(dp(10), dp(8), dp(10), dp(8)) }
            fun option(title: String, checked: Boolean, update: (Boolean) -> Unit) {
                options.addView(android.widget.CheckBox(activity).apply { text = title; textSize = 13f; setTextColor(foreground); buttonTintList = android.content.res.ColorStateList.valueOf(0xff9687f2.toInt()); isChecked = checked; setOnCheckedChangeListener { _, value -> update(value); filter() } })
            }
            option("排除系统应用", excludeSystem) { excludeSystem = it }
            option("排除无法启动的应用", excludeUnlaunchable) { excludeUnlaunchable = it }
            popup?.dismiss()
            popup = android.widget.PopupWindow(options, dp(240), -2, true).apply { setBackgroundDrawable(android.graphics.drawable.ColorDrawable(android.graphics.Color.TRANSPARENT)); elevation = dp(8).toFloat(); showAsDropDown(menu, -dp(196), 0) }
        }
        list.setOnItemClickListener { _, _, position, _ ->
            val item = visible[position]
            pickerBusy = false
            dialog.dismiss()
            importPaths(invoke) {
                val sources = listOf(item.app.sourceDir) + (item.app.splitSourceDirs?.toList() ?: emptyList())
                val output = newImportDirectory()
                val name = "${safeName(item.name)}_${safeName(item.version)}"
                if (sources.size == 1) {
                    val apk = File(output, "$name.apk")
                    File(sources.first()).copyTo(apk)
                    listOf(apk.absolutePath)
                } else {
                    val folder = File(output, name).apply { mkdirs() }
                    sources.forEach { File(it).copyTo(File(folder, File(it).name)) }
                    listOf(folder.absolutePath)
                }
            }
        }
        dialog.setOnCancelListener { pickerBusy = false; resolvePaths(invoke, emptyList()) }
        dialog.setOnDismissListener { popup?.dismiss() }
        dialog.setContentView(body)
        dialog.window?.setBackgroundDrawable(android.graphics.drawable.ColorDrawable(android.graphics.Color.TRANSPARENT))
        dialog.window?.setSoftInputMode(android.view.WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
        dialog.show()
        val available = android.graphics.Rect()
        activity.window.decorView.getWindowVisibleDisplayFrame(available)
        dialog.window?.setLayout(available.width() - dp(24), (available.height() * .94).toInt())
        filter()
    }

    @ActivityCallback
    private fun onPickResult(invoke: Invoke, result: ActivityResult) {
        pickerBusy = false
        if (result.resultCode != Activity.RESULT_OK || result.data == null) {
            pickingDirectory = false
            pickingReceiveDirectory = false
            invoke.resolve(JSObject().apply { put("paths", JSArray()) })
            return
        }
        val directory = pickingDirectory
        val receiveDirectory = pickingReceiveDirectory
        pickingDirectory = false
        pickingReceiveDirectory = false
        importPaths(invoke) {
            val data = result.data!!
            if (receiveDirectory) {
                val uri = data.data ?: throw IllegalStateException("未选择接收目录")
                listOf(authorizeReceiveDirectory(uri, data.flags))
            } else if (directory) data.data?.let { uri ->
                runCatching { activity.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION) }
                copyTree(uri)
            } ?: emptyList() else {
                val uris = linkedSetOf<Uri>()
                data.clipData?.let { clip -> for (index in 0 until clip.itemCount) clip.getItemAt(index).uri?.let(uris::add) }
                data.data?.let(uris::add)
                uris.map { copyUri(it) }
            }
        }
    }

    private fun authorizeReceiveDirectory(uri: Uri, granted: Int): String {
        val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
        check(granted and flags == flags) { "请授予所选目录的读写权限" }
        val resolver = activity.contentResolver
        resolver.takePersistableUriPermission(uri, flags)
        val tree = DocumentFile.fromTreeUri(activity, uri) ?: error("无法打开接收目录")
        check(tree.isDirectory && tree.canRead() && tree.canWrite()) { "所选目录不可读写，请重新选择" }
        val probe = tree.createFile("application/octet-stream", ".nodesend-check-${UUID.randomUUID()}")
            ?: error("无法在所选目录创建文件")
        try {
            resolver.openOutputStream(probe.uri, "wt")?.use { it.write(byteArrayOf(78, 83)) }
                ?: error("所选目录不可写入")
            check(resolver.openInputStream(probe.uri)?.use { it.readBytes().contentEquals(byteArrayOf(78, 83)) } == true) { "所选目录不可读取" }
        } finally { check(probe.delete()) { "无法删除目录权限测试文件" } }
        // Random-access chunks stay in private storage; commit exports only the
        // selected task files through SAF, never a guessed shared-storage path.
        val key = UUID.nameUUIDFromBytes(uri.toString().toByteArray()).toString()
        val staging = File(activity.filesDir, "receive-directories/$key").apply {
            check(mkdirs() || isDirectory) { "无法创建接收暂存目录" }
        }
        check(activity.getSharedPreferences("receive-trees", Context.MODE_PRIVATE).edit()
            .putString(staging.canonicalPath, uri.toString()).putString("id:$key", uri.toString()).commit()) { "无法保存目录授权" }
        return staging.canonicalPath
    }

    private fun receiveTreeUri(root: File): String? {
        val mapping = activity.getSharedPreferences("receive-trees", Context.MODE_PRIVATE)
        mapping.getString(root.canonicalPath, null)?.let { return it }
        if (!root.path.replace('\\', '/').contains("/receive-directories/")) return null
        // Android exposes /data/data and /data/user/0 aliases; UUID keys survive both.
        return mapping.getString("id:${root.name}", null)
            ?: mapping.all.entries.firstOrNull { it.key.endsWith("/receive-directories/${root.name}") }?.value as? String
    }

    @Command
    fun describeReceiveDirectory(invoke: Invoke) {
        val args = invoke.parseArgs(ExportReceivedArgs::class.java)
        runCatching {
            val root = File(args.root).canonicalFile
            val raw = receiveTreeUri(root)
            val label = if (raw == null) {
                if (root.path.contains("/receive-directories/")) "目录授权已丢失，请重新选择" else "应用内存储（请选择公共接收目录）"
            } else {
                val uri = Uri.parse(raw)
                if (uri.authority == "com.android.externalstorage.documents") {
                    val id = android.provider.DocumentsContract.getTreeDocumentId(uri)
                    val volume = id.substringBefore(':')
                    (if (volume == "primary") "/" else "存储卷 $volume /") + id.substringAfter(':', "")
                } else DocumentFile.fromTreeUri(activity, uri)?.name ?: "已授权目录"
            }
            invoke.resolve(JSObject().apply { put("label", label) })
        }.onFailure { invoke.reject("无法读取接收目录：${it.message}") }
    }

    @Command
    fun exportReceived(invoke: Invoke) {
        val args = invoke.parseArgs(ExportReceivedArgs::class.java)
        importWorker.execute {
            runCatching {
                val root = File(args.root).canonicalFile
                val treeUri = receiveTreeUri(root)
                if (treeUri == null) {
                    check(!root.path.contains("/receive-directories/")) { "接收目录授权已丢失，请重新选择目录" }
                    return@runCatching
                }
                val tree = DocumentFile.fromTreeUri(activity, Uri.parse(treeUri)) ?: error("接收目录已不可用")
                check(tree.canRead() && tree.canWrite()) { "接收目录授权失效，请重新选择目录" }
                val receipts = activity.getSharedPreferences("receive-exports", Context.MODE_PRIVATE)
                for (path in args.targets.filterNotNull()) {
                    val source = File(path).canonicalFile
                    check(source.path.startsWith(root.path + File.separator)) { "接收路径越界" }
                    val relative = source.relativeTo(root).invariantSeparatorsPath
                    val receiptKey = args.taskId + ":" + relative
                    val saved = receipts.getString(receiptKey, null)?.let { DocumentFile.fromSingleUri(activity, Uri.parse(it)) }
                    if (saved != null && saved.exists() && saved.length() == source.length()) continue
                    val segments = relative.split('/')
                    var parent = tree
                    for (segment in segments.dropLast(1)) {
                        parent = parent.findFile(segment) ?: parent.createDirectory(segment) ?: error("无法创建接收子目录")
                        check(parent.isDirectory) { "目标路径不是目录" }
                    }
                    val name = segments.last()
                    if (source.isDirectory) {
                        val folder = parent.findFile(name) ?: parent.createDirectory(name) ?: error("无法创建接收目录")
                        check(folder.isDirectory) { "目标目录与文件冲突" }
                        continue
                    }
                    var existing = parent.findFile(name)
                    if (existing != null && args.conflict == "skip") continue
                    var outputName = name
                    if (existing != null && args.conflict == "rename") {
                        var number = 2
                        val fileName = File(name)
                        do {
                            outputName = "${fileName.nameWithoutExtension}-${number++}" + (if (fileName.extension.isEmpty()) "" else ".${fileName.extension}")
                        } while (parent.findFile(outputName) != null)
                        existing = null
                    }
                    val pendingKey = "pending:$receiptKey"
                    val pending = receipts.getString(pendingKey, null)?.let { DocumentFile.fromSingleUri(activity, Uri.parse(it)) }?.takeIf { it.exists() }
                    val mime = android.webkit.MimeTypeMap.getSingleton().getMimeTypeFromExtension(source.extension.lowercase()) ?: "application/octet-stream"
                    val output = pending ?: existing ?: parent.createFile(mime, outputName) ?: error("无法创建接收文件")
                    check(!output.isDirectory) { "目标文件与目录冲突" }
                    check(receipts.edit().putString(pendingKey, output.uri.toString()).commit()) { "无法保存写入状态" }
                    activity.contentResolver.openOutputStream(output.uri, "wt")?.use { sink ->
                        source.inputStream().use { input -> input.copyTo(sink) }
                    } ?: error("无法写入接收文件")
                    check(output.length() == source.length()) { "接收目录文件大小校验失败" }
                    check(receipts.edit().putString(receiptKey, output.uri.toString()).remove(pendingKey).commit()) { "无法保存接收状态" }
                }
            }.onSuccess { invoke.resolve() }
                .onFailure { invoke.reject("无法保存到接收目录：${it.message}") }
        }
    }

    private fun receiveShareIntent(intent: Intent?) {
        if (intent == null || (intent.action != Intent.ACTION_SEND && intent.action != Intent.ACTION_SEND_MULTIPLE)) return
        val uris = linkedSetOf<Uri>()
        intent.clipData?.let { clip -> for (index in 0 until clip.itemCount) clip.getItemAt(index).uri?.let(uris::add) }
        intent.data?.let(uris::add)
        @Suppress("DEPRECATION")
        (intent.getParcelableExtra(Intent.EXTRA_STREAM) as? Uri)?.let(uris::add)
        @Suppress("DEPRECATION")
        intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)?.forEach(uris::add)
        val imported = uris.mapNotNull { uri -> runCatching { copyUri(uri) }.getOrNull() }.toMutableList()
        if (imported.isEmpty()) {
            intent.getStringExtra(Intent.EXTRA_TEXT)?.takeIf { it.isNotBlank() }?.let { text ->
                runCatching { writeSharedText(text) }.getOrNull()?.let(imported::add)
            }
        }
        if (imported.isNotEmpty()) {
            pending.addAll(imported)
            notifySharedFiles(imported)
        }
    }

    private fun notifySharedFiles(paths: List<String>) {
        if (paths.isEmpty()) return
        eventHandler?.send(JSObject().apply { put("paths", JSArray(paths)) })
    }

    private fun copyClipData(clip: ClipData?): List<String> {
        if (clip == null) return emptyList()
        return (0 until clip.itemCount).mapNotNull { index ->
            clip.getItemAt(index).uri?.let { uri -> runCatching { copyUri(uri) }.getOrNull() }
        }
    }

    private fun copyUri(uri: Uri, destination: File = newImportDirectory()): String {
        destination.mkdirs()
        val output = uniqueFile(destination, displayName(uri))
        activity.contentResolver.openInputStream(uri)?.use { input ->
            FileOutputStream(output).use { input.copyTo(it) }
        } ?: throw IllegalStateException("无法读取分享的内容")
        return output.absolutePath
    }

    private fun copyTree(uri: Uri): List<String> {
        val source = DocumentFile.fromTreeUri(activity, uri) ?: throw IllegalStateException("无法读取所选文件夹")
        val root = File(newImportDirectory(), safeName(source.name ?: "folder"))
        copyDocumentTree(source, root)
        return listOf(root.absolutePath)
    }

    private fun copyDocumentTree(source: DocumentFile, destination: File) {
        if (source.isDirectory) {
            destination.mkdirs()
            source.listFiles().forEach { child -> copyDocumentTree(child, File(destination, safeName(child.name ?: "item"))) }
        } else {
            destination.parentFile?.mkdirs()
            activity.contentResolver.openInputStream(source.uri)?.use { input ->
                FileOutputStream(destination).use { input.copyTo(it) }
            } ?: throw IllegalStateException("无法读取文件")
        }
    }

    private fun writeSharedText(text: String): String {
        val file = uniqueFile(newImportDirectory(), "shared-text.txt")
        file.writeText(text)
        return file.absolutePath
    }

    private fun newImportDirectory(): File = File(activity.cacheDir, "nodesend-imports/${UUID.randomUUID()}").apply {
        check(mkdirs() || isDirectory) { "无法创建导入目录" }
    }

    private fun displayName(uri: Uri): String {
        var cursor: Cursor? = null
        return try {
            cursor = activity.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            if (cursor?.moveToFirst() == true) cursor.getString(0)?.let(::safeName) ?: "shared-file" else "shared-file"
        } finally { cursor?.close() }
    }

    private fun uniqueFile(directory: File, name: String): File {
        var candidate = File(directory, safeName(name))
        var index = 2
        while (candidate.exists()) { candidate = File(directory, "${candidate.nameWithoutExtension}-$index${if (candidate.extension.isBlank()) "" else ".${candidate.extension}"}"); index++ }
        return candidate
    }

    private fun formatBytes(value: Long): String = if (value < 1024 * 1024) "${value / 1024} KB" else "${value / (1024 * 1024)} MB"

    private fun safeName(name: String): String = name.replace(Regex("[\\\\/:*?\"<>|]"), "_").ifBlank { "shared-file" }
    private fun readFlags() = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
}
