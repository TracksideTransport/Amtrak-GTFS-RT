# Licenses of what is in this image

`amtrak-gtfs-rt` is AGPL-3.0-only (`AGPL-3.0-only.txt`). It also contains the software below, each
under its own license. This file says where each license text lives, so the image alone is enough
to find them.

| Component | License text in the image |
| --- | --- |
| Rust crates linked into `amtrak-gtfs-rt-service` | `/licenses/THIRD_PARTY_LICENSES.html` |
| MobilityData GTFS validator 8.0.1 (the JAR and its shaded dependencies) | `/licenses/gtfs-validator-LICENSE` (Apache-2.0); dependency notices are inside the JAR under `META-INF/` |
| Eclipse Temurin / OpenJDK 17 runtime (`/opt/java`) | `/opt/java/legal/<module>/` (GPLv2 with the Classpath Exception, and third-party notices per module) |
| Debian packages (glibc, Chromium's shared libraries, fonts, `tini`, and the rest of the base) | `/usr/share/doc/<package>/copyright` |
| Python 3.12 | `/usr/local/lib/python3.12/LICENSE.txt` |
| Python packages (`playwright`, `cryptography`, `greenlet`, `pyee`, `cffi`, and their dependencies) | `/usr/local/lib/python3.12/site-packages/<package>-<version>.dist-info/` (`LICENSE*` files, or `licenses/`) |
| Chromium headless shell and its bundled libraries | `/ms-playwright/chromium_headless_shell-*/*/LICENSE.headless_shell` |
| FFmpeg (installed by Playwright beside the browser) | `/ms-playwright/ffmpeg-*/COPYING.LGPLv2.1` |

`scripts/verify-release-image.sh` checks that these files are present in every release image.
