# Plan: rebrand Zeron -> Zerona (branding only)

Status: DRAFT, menunggu konfirmasi keputusan di bagian "Keputusan terbuka".
Fork: pancaprima/zeron (upstream zeronsh/zeron, lisensi MIT, Copyright 2026 Wing).

## Tujuan
Ganti nama yang terlihat user menjadi **Zerona**, tanpa memutus sync/login ke backend upstream
dan tanpa membuat merge dari upstream jadi mimpi buruk.

## Prinsip
- Diff sekecil mungkin: JANGAN rename folder/crate internal (`apps/zeron`, `crates/*`, `apps/ios/Zeron*`).
- Jangan sentuh kontrak dengan backend upstream: `edge.zeron.sh`, WorkOS client id, skema URL `zeron://`
  (redirect OAuth terdaftar atas nama upstream), bundle id iOS `sh.zeron.ios`, env var `ZERON_*`.
- LICENSE tetap utuh; README fork menyebut atribusi ke upstream.

## Ruang lingkup (IN)
1. Binary: `[[bin]] name` di `apps/zeron/Cargo.toml` jadi `zerona` (package dir tetap `apps/zeron`).
2. Teks CLI yang tampil ke user (help, pesan error, "zeron daemon is only supported...").
3. systemd unit `zeron.service` -> `zerona.service` (`SYSTEMD_UNIT` di `daemon.rs`), path ExecStart.
4. Data dir default `~/.zeron` -> `~/.zerona`, dengan **fallback**: kalau `~/.zerona` belum ada tapi `~/.zeron` ada,
   pakai `~/.zeron` (tidak pernah memindahkan data otomatis). Termasuk layout `app/<ver>` + `current`
   yang dipakai installer dan updater (`crates/update/src/lib.rs`).
5. Nama tampil di UI/desktop: `dist/zeron.desktop` (Name, Exec, TryExec, Icon, StartupWMClass), judul window,
   nama app bundle macOS (dist/macos), ikon bila ada.
6. README fork: nama baru + bagian "Forked from zeronsh/zeron" + catatan perbedaan (content search, browser persistence).

## Di luar lingkup (OUT, sengaja tidak diubah)
- Nama crate/folder, namespace Rust, `ZERON_*` env, `zeron://`, `sh.zeron.ios`, `sh.zeron.app` (launchd label),
  `edge.zeron.sh`, WorkOS, aplikasi iOS, folder `apps/landing`, `apps/www-redirect`, docs riset.
- Perubahan ini sengaja bisa ditambah nanti kalau backend sendiri sudah ada.

## RISIKO PENTING
1. **Updater menunjuk rilis upstream** (`RELEASES_PAGE` / `LATEST_RELEASE_PAGE` = github.com/zeronsh/zeron).
   Kalau tidak diarahkan ulang ke fork, `zerona update` bisa menimpa binary kita (dengan content search dan
   browser persistence) dengan build upstream. Harus diarahkan ke `pancaprima/zeron` releases atau dimatikan.
2. **Data dir**: `session.json` (login) dan `device-id` ada di `~/.zeron`. Salah pindah = device terdaftar baru
   di workspace. Karena itu fallback + backup, bukan auto-move.
3. **Verifikasi build**: VPS cuma 1 vCPU / 3.8 GB, build workspace Rust penuh berat. Rencana: `cargo check -p zeron`
   (atau paket terkecil yang mencakup perubahan) di VPS, sisanya via CI GitHub Actions bila workflow aktif
   (cek `actions/workflows` dulu, jangan janji sebelum dicek).
4. Merge dari upstream: perubahan terpusat di sedikit file (Cargo.toml bin, daemon.rs, update/lib.rs, dist/*, README),
   jadi konflik minim.

## Fase A: kode (didelegasikan ke Cursor via cursor-me)
- Branch `feat/rebrand-zerona` dari `main`, PR **draft** ke `main` fork sendiri (bukan upstream).
- Ubah hanya file di daftar IN. Tambah test untuk fallback data dir dan nama unit systemd.
- Bukti wajib: `git diff --stat`, `cargo check` / test yang relevan benar-benar jalan, hasil nyata dilaporkan.

## Fase B: operasional VPS (setelah PR direview, butuh oke user)
1. Backup `~/.zeron` (tar, perms 0600) ke direktori backup, catat device-id sebelum dan sesudah.
2. Build/install binary `zerona`, stop `zeron.service`, enable `zerona.service`, `zerona status` dan `zerona sync`
   harus menunjukkan `Mode: synced` dengan device-id yang sama.
3. Rollback: stop `zerona`, start `zeron` lama (data dir tidak dipindah, jadi aman).
4. Rename repo GitHub `pancaprima/zeron` -> `pancaprima/zerona` (`gh repo rename`; GitHub otomatis redirect),
   lalu update remote clone lokal dan skill `zeron-headless-ops`.

## Keputusan terbuka
1. Data dir: (a) `~/.zerona` dengan fallback ke `~/.zeron` [disarankan], (b) tetap `~/.zeron` dulu.
2. Updater: (a) arahkan ke releases `pancaprima/zeron` [disarankan], (b) nonaktifkan sementara.
3. Nama repo GitHub: rename ke `zerona` di akhir Fase B, atau tetap `zeron`.

## Perubahan arah (Fase A2): nama file binary TETAP `zeron`
Keputusan user: opsi 3. Setelah Fase A, ketahuan bahwa mengganti `[[bin]]` jadi `zerona` mematahkan banyak script
paket, CI, installer, dan nama artefak rilis di semua platform. Maka nama file binary dikembalikan ke `zeron`.
Branding Zerona tinggal di: teks tampilan (nama produk, README, deskripsi), `dist/zeron.desktop` (Name=Zerona,
Exec/TryExec/Icon/StartupWMClass tetap `zeron`), unit systemd `zerona.service`, data dir `~/.zerona` (fallback `~/.zeron`),
dan updater ke releases fork.
Contoh perintah di teks CLI memakai `zeron` (nama binary sebenarnya); nama produk yang tampil memakai "Zerona".
Managed install: `<data-root>/app/current/zeron` (data-root = ~/.zerona atau fallback ~/.zeron).
Tidak menyentuh: script paket, CI, installer upstream, Info.plist, nama artefak rilis.
Verifikasi: tanpa build workspace/crates/ui; `cargo test -p zeron-update`, `rustfmt --check`, review.

## Fase B1: rilis lewat fork (disetujui user: opsi 1 + rename repo)
Status: PR #4 sudah MERGED ke main (CI hijau). Temuan saat cek workflow:
- `release.yml` sudah bikin GitHub Release (tarball + `manifest.json`) saat tag `v<versi>` di-push. Langkah upload ke R2
  hanya jalan bila `CLOUDFLARE_API_TOKEN` ada; fork tidak punya, jadi otomatis ter-skip (tidak menyentuh bucket upstream).
- Updater membaca `{edge}/releases` (upstream) KECUALI env `ZERON_RELEASES_URL` diisi (wajib https, tanpa query).
  Jadi TIDAK perlu ubah kode: cukup isi `ZERON_RELEASES_URL=https://github.com/pancaprima/zeron/releases/latest/download`
  di file env service (`~/.zeron/env` / `~/.zerona/env`). GitHub me-redirect ke aset `manifest.json` dan tarball.
- Tag harus sama dengan versi di `Cargo.toml` (sekarang 0.2.107). Naikkan ke 0.2.108 supaya terlihat lebih baru dari binary managed.
- Build rilis mencakup Linux x86_64/aarch64, macOS, Windows (semua wajib sukses sebelum `publish`). Perkiraan 20-40 menit.

Langkah:
1. Branch `chore/release-0.2.108`: bump versi workspace di `Cargo.toml` (+ `Cargo.lock`), PR, CI hijau, merge.
2. Tag `v0.2.108` di main, push tag, pantau `release` workflow sampai `publish` sukses.
3. Verifikasi aset rilis: tarball Linux x86_64 + `manifest.json` (sha256 cocok).
4. Lanjut Fase B (backup `~/.zeron`, pasang binary dari rilis, stop zeron.service, enable zerona.service, cek device-id sama).
5. Rename repo ke `pancaprima/zerona` SETELAH rilis terpasang dan stabil (GitHub redirect URL lama, tapi URL feed di env
   sebaiknya diperbarui ke nama baru).
Rollback: tag/rilis bisa dihapus; service lama tetap utuh karena data dir tidak dipindah.
