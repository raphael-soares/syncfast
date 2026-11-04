# Third-party notices

## rclone

The Flatpak bundles the official rclone release binary (version pinned in `packaging/flatpak/io.github.raphael_soares.syncfast.yml`, unmodified), distributed under the MIT license. syncfast runs it as a separate program. Source: <https://github.com/rclone/rclone>. rclone's own dependencies carry their licenses in its release; see <https://rclone.org/licence/>.

```text
Copyright (C) 2012 by Nick Craig-Wood http://www.craig-wood.com/nick/

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

## Lucide icons

The SVG icons in `crates/syncfast-gui/assets/icons/`, and the glyph inside the app icon (`crates/syncfast-gui/assets/app-icon/syncfast.svg`, copied to `assets/logo.svg`), come from [Lucide](https://lucide.dev), distributed under the ISC license. Some Lucide icons derive from Feather (MIT). The full license texts, including Feather's, are in `crates/syncfast-gui/assets/icons/LICENSE.txt`.

```text
ISC License

Copyright (c) 2026 Lucide Icons and Contributors

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

## Inter

The interface font Inter comes through the `fastframe-fonts` crate and is distributed under the SIL Open Font License 1.1. Source: <https://github.com/rsms/inter>.

## Rust crates

Rust dependencies are listed in `Cargo.lock`. `cargo deny check` accepts only the licenses allowed in `deny.toml` (MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0, OFL-1.1 and similar permissive ones).
