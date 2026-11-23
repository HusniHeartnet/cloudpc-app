"""Buat ikon aplikasi Android dari logo CloudPC (client/assets/cloudpc.icns, 1024 px).

Dipanggil build-android.ps1 tiap build, supaya ikon bawaan Tauri tidak pernah terbawa lagi. Butuh Pillow.

- Ikon lama (Android < 8 dan beberapa peluncur): mipmap-*/ic_launcher.png dan ic_launcher_round.png = logo apa adanya.
- Ikon adaptif (Android 8+): latar biru logo (#2563EB) + mipmap-*/ic_launcher_foreground.png. Latar depan = logo yang
  diperbesar sampai kotak birunya memenuhi seluruh kanvas 108 dp, jadi awannya jatuh di zona aman 66 dp dan peluncur
  bebas memotong bentuknya (bulat, squircle) tanpa memotong awan.
"""
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
LOGO = ROOT / "client" / "assets" / "cloudpc.icns"
RES = ROOT / "client-rs" / "gen" / "android" / "app" / "src" / "main" / "res"
BACKGROUND = "#2563EB"
# kerapatan -> (ikon lama px, latar depan adaptif px)
SIZES = {"mdpi": (48, 108), "hdpi": (72, 162), "xhdpi": (96, 216), "xxhdpi": (144, 324), "xxxhdpi": (192, 432)}

ADAPTIVE = """<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@color/ic_launcher_background" />
    <foreground android:drawable="@mipmap/ic_launcher_foreground" />
</adaptive-icon>
"""
COLOR = f"""<?xml version="1.0" encoding="utf-8"?>
<resources>
    <color name="ic_launcher_background">{BACKGROUND}</color>
</resources>
"""


def main() -> int:
    if not RES.is_dir():
        print(f"folder res Android tidak ada: {RES}", file=sys.stderr)
        return 1
    logo = Image.open(LOGO).convert("RGBA")
    square = logo.crop(logo.getbbox())  # kotak biru tanpa tepi transparan
    for density, (legacy, adaptive) in SIZES.items():
        folder = RES / f"mipmap-{density}"
        folder.mkdir(parents=True, exist_ok=True)
        icon = logo.resize((legacy, legacy), Image.LANCZOS)
        icon.save(folder / "ic_launcher.png")
        icon.save(folder / "ic_launcher_round.png")
        # Sedikit diperkecil (88%) supaya ujung awan pasti di dalam zona aman; sudut kotak yang jadi transparan tertutup
        # latar berwarna sama.
        inner = round(adaptive * 0.88)
        foreground = Image.new("RGBA", (adaptive, adaptive), (0, 0, 0, 0))
        foreground.paste(square.resize((inner, inner), Image.LANCZOS), ((adaptive - inner) // 2, (adaptive - inner) // 2))
        foreground.save(folder / "ic_launcher_foreground.png")
    anydpi = RES / "mipmap-anydpi-v26"
    anydpi.mkdir(parents=True, exist_ok=True)
    (anydpi / "ic_launcher.xml").write_text(ADAPTIVE, encoding="utf-8")
    (anydpi / "ic_launcher_round.xml").write_text(ADAPTIVE, encoding="utf-8")
    (RES / "values" / "ic_launcher_background.xml").write_text(COLOR, encoding="utf-8")
    print(f"ikon Android dibuat dari {LOGO.name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
