# Third-party notices

RyukinLedger's capture core is derived from other people's work.  Every
component listed here is under the MIT licence, which requires that the
copyright notice and the permission notice travel with copies of the software --
so this file has to stay in the repository, and it is copied next to the
binaries by `tools/build.ps1`.

## Derived from

| Component | Licence | Copyright |
| --- | --- | --- |
| [konkers/irminsul](https://github.com/konkers/irminsul) | MIT | © 2025 Erik Gilling |

`src/irminsul/` is a modified copy of that project: the interface, the ledger, the
balance tracker and the wire parser were replaced or rewritten, and the parts
that only existed to export data for Genshin Optimizer were removed.  The
original licence text is kept verbatim at [`src/irminsul/LICENSE`](src/irminsul/LICENSE).

## Linked into `irminsul.exe`

| Component | Licence | Copyright |
| --- | --- | --- |
| [konkers/auto-artifactarium](https://github.com/konkers/auto-artifactarium) | MIT | © 2024 IceDynamix |
| [hashblen/mhy-kcp](https://github.com/hashblen/mhy-kcp) | MIT | © 2017 Zhang Cheng |
| [pktmon](https://crates.io/crates/pktmon) | MIT | © 2025 emmachase |

## Other Rust dependencies

Compiled into `irminsul.exe` from crates.io.  Versions are pinned in
`src/irminsul/Cargo.lock`.

| Crate | Licence |
| --- | --- |
| anyhow, async-trait, base64, chrono, futures, serde_json, tracing, tracing-appender | MIT OR Apache-2.0 |
| clap, serde, windows | MIT OR Apache-2.0 |
| tokio, tracing-subscriber | MIT |

The dual-licensed crates are used under the MIT option, so one licence text
covers everything here.

## The interface

`src/RyukinLedger.App` uses no NuGet packages: WPF and WinForms both come from
the .NET runtime.  Nothing third-party is linked into `RyukinLedger.exe`.

## MIT licence text

```
MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

Each component's own copyright line is the one listed in the tables above.
