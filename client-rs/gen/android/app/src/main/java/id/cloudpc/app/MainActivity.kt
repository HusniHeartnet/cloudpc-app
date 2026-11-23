package id.cloudpc.app

import android.graphics.Color
import android.os.Bundle
import android.view.View
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    // Android 15+ menggambar aplikasi sampai ke balik bilah status dan bilah navigasi. Bilahnya dibuat transparan dengan
    // ikon terang (aplikasi bertema gelap) ...
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
      navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
    )
    super.onCreate(savedInstanceState)
    // ... dan isi aplikasi diberi jarak sebesar bilah-bilah itu, poni kamera, dan keyboard layar. Tanpa ini tombol
    // Beranda/Akun di bawah menempel ke tombol kembali/home HP dan judul di atas menempel ke jam/sinyal (laporan pemilik
    // 23 Nov 2026), dan keyboard menutupi kolom yang sedang diisi.
    val content = findViewById<View>(android.R.id.content)
    content.setBackgroundColor(0xFF0E1014.toInt())
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val bars = insets.getInsets(
        WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout() or WindowInsetsCompat.Type.ime()
      )
      view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      WindowInsetsCompat.CONSUMED
    }
  }
}
