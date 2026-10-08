package com.picdupe.nodesend

import android.os.Bundle
import android.os.Build
import android.provider.Settings
import android.system.Os
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    // Publish the system name before super starts the native Tauri service.
    val systemName = runCatching {
      Settings.Global.getString(contentResolver, "device_name")
    }.getOrNull()?.trim()?.takeIf { it.isNotEmpty() }
    val deviceName = systemName ?: Build.MODEL.trim().ifEmpty { "Android" }
    Os.setenv("NODESEND_ANDROID_DEVICE_NAME", deviceName, true)
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
  }
}
