# Maintainer: Your Name <you@example.com>
pkgname=usbackup
pkgver=1.0.0
pkgrel=1
pkgdesc="Automated USB backup agent: detects USB keys and runs signed rsync backups"
arch=('x86_64')
url="https://github.com/yourname/USBackup"
license=('MIT')
makedepends=('rust' 'cargo')
source=("https://github.com/yourname/USBackup/archive/v${pkgver}.tar.gz")
sha256sums=('SKIP')

build() {
  cd "$pkgname-$pkgver"
  cargo build --release --frozen
}

package() {
  cd "$pkgname-$pkgver"
  cargo install --root "$pkgdir/usr" --path . --frozen --bins
  mv "$pkgdir/usr/bin/USBackup" "$pkgdir/usr/bin/usbackup"
}
