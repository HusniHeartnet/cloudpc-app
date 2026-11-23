package id.cloudpc.app

import android.app.Activity
import android.content.Intent
import androidx.core.content.FileProvider
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import id.cloudpc.stream.Native
import id.cloudpc.stream.StreamActivity
import java.io.File
import kotlin.concurrent.thread

/**
 * Jembatan aplikasi CloudPC (Rust, `client-rs/src/android_stream.rs`) ke komponen layar PC `id.cloudpc.stream`
 * (sumber di `stream-rs/android`). Aplikasi hanya meminta: pairing, buka layar, tanya keadaan, tutup.
 */
@TauriPlugin
class StreamPlugin(private val activity: Activity) : Plugin(activity) {
    /** Pairing dengan PIN; menjawab `{error}` (kosong = berhasil) setelah PC menerima PIN atau gagal. */
    @Command
    fun pair(invoke: Invoke) {
        val args = invoke.getArgs()
        val host = args.optString("host")
        val pin = args.optString("pin")
        thread(name = "cloudpc-pair") {
            val error = try {
                Native.pair(StreamActivity.dataDir(activity), host, pin)
            } catch (e: Throwable) {
                "Pairing gagal: ${e.message ?: e.javaClass.simpleName}"
            }
            invoke.resolve(JSObject().apply { put("error", error) })
        }
    }

    /** Buka layar stream (mendatar, layar penuh) di atas aplikasi. */
    @Command
    fun start(invoke: Invoke) {
        val args = invoke.getArgs()
        val intent = Intent(activity, StreamActivity::class.java)
            .putExtra(StreamActivity.EXTRA_HOST, args.optString("host"))
            .putExtra(StreamActivity.EXTRA_APP, args.optString("app", "Desktop"))
            .putExtra(StreamActivity.EXTRA_WIDTH, args.optInt("width", 1280))
            .putExtra(StreamActivity.EXTRA_HEIGHT, args.optInt("height", 720))
            .putExtra(StreamActivity.EXTRA_FPS, args.optInt("fps", 60))
            .putExtra(StreamActivity.EXTRA_BITRATE_KBPS, args.optInt("bitrateKbps", 8000))
            .putExtra(StreamActivity.EXTRA_CODEC, args.optString("codec", "auto"))
            .putExtra(StreamActivity.EXTRA_LOG_PATH, args.optString("logPath"))
        StreamActivity.markStarting()
        activity.runOnUiThread {
            try {
                activity.startActivity(intent)
                invoke.resolve(JSObject())
            } catch (e: Throwable) {
                StreamActivity.close()
                invoke.reject("Layar PC tidak bisa dibuka: ${e.message ?: e.javaClass.simpleName}")
            }
        }
    }

    /** `{running, error}`: apakah layar stream masih ada, dan pesan galat terakhirnya. */
    @Command
    fun state(invoke: Invoke) {
        invoke.resolve(JSObject().apply {
            put("running", StreamActivity.running)
            put("error", StreamActivity.lastError)
        })
    }

    /**
     * Pasang APK pembaruan yang sudah diunduh aplikasi (SHA-256 sudah dicek di sisi Rust). Android tidak mengizinkan
     * pemasangan diam-diam: yang dibuka adalah pemasang sistem, dan pemain menekan "Update" di sana. Pertama kali, Android
     * juga meminta izin "pasang aplikasi dari sumber ini" untuk CloudPC.
     */
    @Command
    fun installApk(invoke: Invoke) {
        try {
            val source = File(invoke.getArgs().optString("path"))
            if (!source.isFile) throw IllegalStateException("file pembaruan tidak ditemukan")
            // Pemasang sistem membaca berkas lewat FileProvider, yang hanya membagikan folder cache (res/xml/file_paths.xml).
            val dir = File(activity.cacheDir, "updates").apply { mkdirs() }
            val apk = File(dir, "CloudPC-update.apk")
            source.copyTo(apk, overwrite = true)
            val uri = FileProvider.getUriForFile(activity, activity.packageName + ".fileprovider", apk)
            val intent = Intent(Intent.ACTION_VIEW)
                .setDataAndType(uri, "application/vnd.android.package-archive")
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
            activity.runOnUiThread {
                try {
                    activity.startActivity(intent)
                    invoke.resolve(JSObject())
                } catch (e: Throwable) {
                    invoke.reject("Pemasang Android tidak bisa dibuka: ${e.message ?: e.javaClass.simpleName}")
                }
            }
        } catch (e: Throwable) {
            invoke.reject("Pembaruan tidak bisa dipasang: ${e.message ?: e.javaClass.simpleName}")
        }
    }

    @Command
    fun stop(invoke: Invoke) {
        StreamActivity.close()
        invoke.resolve(JSObject())
    }
}
