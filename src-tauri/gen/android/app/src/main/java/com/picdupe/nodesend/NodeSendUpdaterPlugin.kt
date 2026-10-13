package com.picdupe.nodesend

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.core.content.FileProvider
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import java.util.Locale
import java.util.concurrent.Executors

@InvokeArg
class UpdateChannelArgs { var channel: String = "stable" }

@InvokeArg
class DownloadUpdateArgs { lateinit var url: String; lateinit var sha256: String }

@InvokeArg
class InstallUpdateArgs { lateinit var path: String }

@TauriPlugin
class NodeSendUpdaterPlugin(private val activity: android.app.Activity) : Plugin(activity) {
    private val worker = Executors.newSingleThreadExecutor()
    private val stableManifest = "https://raw.githubusercontent.com/picdupe/NodeSend/main/updates/stable/latest.json"
    private val testManifest = "https://raw.githubusercontent.com/picdupe/NodeSend/main/updates/test/latest.json"

    @Command
    fun checkUpdate(invoke: Invoke) {
        val channel = invoke.parseArgs(UpdateChannelArgs::class.java).channel
        if (channel != "stable" && channel != "test") { invoke.reject("更新通道无效。"); return }
        worker.execute {
            try {
                val manifestUrl = if (channel == "stable") stableManifest else testManifest
                val payload = JSONObject(readHttps(manifestUrl, 1024 * 1024).toString(Charsets.UTF_8))
                val androidUpdate = payload.optJSONObject("android")
                    ?: throw IllegalStateException("该更新通道尚未发布 Android 更新信息。")
                val version = androidUpdate.optString("version", payload.optString("version"))
                val versionCode = androidUpdate.optLong("versionCode", -1L)
                val downloadUrl = androidUpdate.optString("url", "")
                val sha256 = androidUpdate.optString("sha256", "").lowercase(Locale.ROOT)
                if (version.isBlank() || versionCode <= 0 || !downloadUrl.startsWith("https://") || !sha256.matches(Regex("[0-9a-f]{64}"))) {
                    throw IllegalStateException("Android 更新清单缺少有效的版本号、下载地址或 SHA-256。")
                }
                val packageInfo = if (Build.VERSION.SDK_INT >= 33) {
                    activity.packageManager.getPackageInfo(activity.packageName, android.content.pm.PackageManager.PackageInfoFlags.of(0))
                } else {
                    @Suppress("DEPRECATION")
                    activity.packageManager.getPackageInfo(activity.packageName, 0)
                }
                val currentCode = if (Build.VERSION.SDK_INT >= 28) packageInfo.longVersionCode else legacyVersionCode(packageInfo)
                if (versionCode <= currentCode) {
                    invoke.resolve(JSObject().put("updateAvailable", false).put("currentVersion", packageInfo.versionName ?: "").put("currentVersionCode", currentCode))
                } else {
                    invoke.resolve(JSObject().put("updateAvailable", true).put("version", version).put("versionCode", versionCode)
                        .put("currentVersion", packageInfo.versionName ?: "").put("currentVersionCode", currentCode)
                        .put("notes", payload.optString("notes", "")).put("downloadUrl", downloadUrl).put("sha256", sha256))
                }
            } catch (e: Exception) { invoke.reject(e.message ?: "检查 Android 更新失败。") }
        }
    }

    @Command
    fun downloadUpdate(invoke: Invoke) {
        val args = invoke.parseArgs(DownloadUpdateArgs::class.java)
        if (!args.url.startsWith("https://") || !args.sha256.matches(Regex("(?i)[0-9a-f]{64}"))) {
            invoke.reject("更新下载地址或 SHA-256 无效。"); return
        }
        worker.execute {
            val temp = File(activity.cacheDir, "nodesend-update.apk.part")
            try {
                val connection = URL(args.url).openConnection() as HttpURLConnection
                connection.connectTimeout = 15000
                connection.readTimeout = 30000
                connection.instanceFollowRedirects = true
                try {
                    connection.connect()
                    if (connection.url.protocol != "https") throw IllegalStateException("下载地址重定向到非 HTTPS 地址，已拒绝。")
                    if (connection.responseCode !in 200..299) throw IllegalStateException("下载更新失败：HTTP ${connection.responseCode}")
                    if (connection.contentLengthLong > 1024L * 1024 * 1024) throw IllegalStateException("APK 文件超过 1 GiB，已拒绝下载。")
                    connection.inputStream.use { input -> FileOutputStream(temp).use { output ->
                        val buffer = ByteArray(64 * 1024); var total = 0L
                        while (true) { val count = input.read(buffer); if (count < 0) break; total += count
                            if (total > 1024L * 1024 * 1024) throw IllegalStateException("APK 文件超过 1 GiB，已停止下载。")
                            output.write(buffer, 0, count)
                        }
                    } }
                } finally { connection.disconnect() }
                val messageDigest = MessageDigest.getInstance("SHA-256")
                temp.inputStream().buffered().use { input ->
                    val buffer = ByteArray(64 * 1024)
                    while (true) { val count = input.read(buffer); if (count < 0) break; messageDigest.update(buffer, 0, count) }
                }
                val digest = messageDigest.digest().joinToString("") { "%02x".format(it) }
                if (!digest.equals(args.sha256, ignoreCase = true)) throw IllegalStateException("APK SHA-256 校验失败，已拒绝安装。")
                val apk = File(activity.cacheDir, "nodesend-update.apk")
                if (apk.exists() && !apk.delete()) throw IllegalStateException("无法替换缓存中的旧更新包。")
                if (!temp.renameTo(apk)) throw IllegalStateException("无法保存下载完成的 APK。")
                invoke.resolve(JSObject().put("path", apk.absolutePath))
            } catch (e: Exception) { temp.delete(); invoke.reject(e.message ?: "下载 Android 更新失败。") }
        }
    }

    @Command
    fun installUpdate(invoke: Invoke) {
        val path = invoke.parseArgs(InstallUpdateArgs::class.java).path
        val apk = File(path).canonicalFile
        if (apk.parentFile != activity.cacheDir.canonicalFile || !apk.isFile || apk.name != "nodesend-update.apk") {
            invoke.reject("更新包路径无效。"); return
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && !activity.packageManager.canRequestPackageInstalls()) {
            activity.startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}")))
            invoke.reject("请在系统设置中允许 NodeSend 安装未知应用，然后返回再次点击安装更新。")
            return
        }
        try {
            val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", apk)
            val intent = Intent(Intent.ACTION_VIEW).setDataAndType(uri, "application/vnd.android.package-archive")
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
            activity.startActivity(intent)
            invoke.resolve()
        } catch (e: Exception) { invoke.reject(e.message ?: "无法启动 Android 系统安装器。") }
    }

    private fun readHttps(address: String, maxBytes: Int): ByteArray {
        val connection = URL(address).openConnection() as HttpURLConnection
        connection.connectTimeout = 12000
        connection.readTimeout = 15000
        connection.instanceFollowRedirects = true
        try {
            connection.connect()
            if (connection.url.protocol != "https") throw IllegalStateException("更新源重定向到非 HTTPS 地址，已拒绝。")
            if (connection.responseCode !in 200..299) throw IllegalStateException("更新源返回 HTTP ${connection.responseCode}。")
            if (connection.contentLengthLong > maxBytes) throw IllegalStateException("更新清单过大。")
            return connection.inputStream.use { input ->
                val output = java.io.ByteArrayOutputStream()
                val buffer = ByteArray(8192)
                while (true) {
                    val count = input.read(buffer)
                    if (count < 0) break
                    if (output.size() + count > maxBytes) throw IllegalStateException("更新清单过大。")
                    output.write(buffer, 0, count)
                }
                output.toByteArray()
            }
        } finally { connection.disconnect() }
    }

    @Suppress("DEPRECATION")
    private fun legacyVersionCode(info: android.content.pm.PackageInfo): Long = info.versionCode.toLong()
}
