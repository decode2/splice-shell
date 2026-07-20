# Changelog

## [0.3.0](https://github.com/decode2/splice-shell/compare/v0.2.0...v0.3.0) (2026-07-20)


### Features

* **compat:** add AI CLI compatibility harness ([42fcaf3](https://github.com/decode2/splice-shell/commit/42fcaf3ddf6d8a111c8c8af4a4e489dc55bbfc52))
* **compat:** add finite AI CLI compatibility harness ([1c02999](https://github.com/decode2/splice-shell/commit/1c02999a75f2e120f971648f05535dc4c0640287))
* **platform:** add target-aware desktop services ([0ac54b8](https://github.com/decode2/splice-shell/commit/0ac54b85523852836e2f4ba273390883eca51841))
* **platform:** add target-aware desktop services ([bd373ab](https://github.com/decode2/splice-shell/commit/bd373ab5f886c5ecddddf2bda89e95c5292b8eb5))
* **platform:** enforce Tauri command authority ([1843218](https://github.com/decode2/splice-shell/commit/18432184d4466e5c4c3b80e89f12ca40520f13de))
* **platform:** enforce Tauri command authority ([85ecd1c](https://github.com/decode2/splice-shell/commit/85ecd1cb02d8b6ed6f412e0102652735eb9d01ba))
* **platform:** wire target-aware PTY sessions ([be9cbc7](https://github.com/decode2/splice-shell/commit/be9cbc7ae08eb99b96eb0de880190b36afb8816e))
* **platform:** wire target-aware PTY sessions ([cfa2254](https://github.com/decode2/splice-shell/commit/cfa2254f9e44b2aeb3db26ebce97b053c7543079))
* **pty:** add platform-neutral session contract ([23af77a](https://github.com/decode2/splice-shell/commit/23af77a0659f1c51c9ed28b596cadfb9dcad5710))
* **pty:** add platform-neutral session contract ([cf7306b](https://github.com/decode2/splice-shell/commit/cf7306b01114a66b7c53641ef8546ba1f4b31d62))
* **pty:** add Unix PTY spawn and I/O ([4acd330](https://github.com/decode2/splice-shell/commit/4acd330acae69282b2b652454a21227ca9a546d4))
* **pty:** add Unix PTY spawn and I/O ([f0a2baf](https://github.com/decode2/splice-shell/commit/f0a2baf6f59c67d04e124d84cb437d438b32878c))
* **pty:** harden Unix PTY lifecycle ([7a3b65d](https://github.com/decode2/splice-shell/commit/7a3b65d2f96beee37c9b506af101b4cb24bd99c5))
* **pty:** harden Unix PTY lifecycle ([f18e59d](https://github.com/decode2/splice-shell/commit/f18e59d6ca0f69f0c5de62dc344844fa7a628362))
* **workspace:** add lifecycle controller ([907bb8b](https://github.com/decode2/splice-shell/commit/907bb8ba6c4050c5b1e3ba73a1ff61e4a18900c3))
* **workspace:** add lifecycle controller ([4c3b487](https://github.com/decode2/splice-shell/commit/4c3b487e836196232d2258e1530d9d0eb30dbf87))
* **workspace:** add lifecycle switcher ([f1506d5](https://github.com/decode2/splice-shell/commit/f1506d5c3b9b859acf3556a7f3721820bf8ffc19))
* **workspace:** add lifecycle switcher ([f514911](https://github.com/decode2/splice-shell/commit/f51491180dc448d86c18ce3898f10ed1beea62a8))
* **workspace:** add local profile persistence ([18adad9](https://github.com/decode2/splice-shell/commit/18adad95eb1d8ed83d22b5fb5911042871a83081))
* **workspace:** add local profile persistence ([e2e60fb](https://github.com/decode2/splice-shell/commit/e2e60fbb5c78feb853aceccce65b3ef4da565580))
* **workspace:** add typed desktop client ([fc1aeb2](https://github.com/decode2/splice-shell/commit/fc1aeb24da1403d3e72bab4e1c92cafab18ef2e9))
* **workspace:** add typed desktop client ([7ae5b6b](https://github.com/decode2/splice-shell/commit/7ae5b6b9dfa6e8227de218ac8bc900907c3d7dca))
* **workspace:** expose lifecycle command authority ([e15a427](https://github.com/decode2/splice-shell/commit/e15a427ae3326a05484fe70305a8f529f59ca553))
* **workspace:** expose lifecycle command authority ([29d1b64](https://github.com/decode2/splice-shell/commit/29d1b64339874abb86255c60e1294692070c807d))
* **workspace:** negotiate output adoption ([7c8a3d0](https://github.com/decode2/splice-shell/commit/7c8a3d0b141c35a8ddd41aa08ab3f3547b1718df))
* **workspace:** negotiate output adoption protocol ([21c6c51](https://github.com/decode2/splice-shell/commit/21c6c512534242936a0d5eedc7223a193d75d031))
* **workspace:** wire lifecycle commands ([753d0b8](https://github.com/decode2/splice-shell/commit/753d0b8fae9f3f292804c1ad7cbbbfb5d3e91153))
* **workspace:** wire lifecycle commands ([e9f521d](https://github.com/decode2/splice-shell/commit/e9f521d52c6bf83b8432ce4a681b1531c44d098f))


### Bug Fixes

* **compat:** satisfy Windows Clippy ([e9269a9](https://github.com/decode2/splice-shell/commit/e9269a9c9aa4d84100541682e417d2456723d6ac))
* **workspace:** preserve adopted sessions in strict mode ([5efe328](https://github.com/decode2/splice-shell/commit/5efe328d77f86cfc641c0803e2419e9d5ccddac1))
* **workspace:** preserve adopted sessions in StrictMode ([c99bf49](https://github.com/decode2/splice-shell/commit/c99bf4953117aeaf707a5f5cc12201fea18c158e))

## [0.2.0](https://github.com/decode2/splice-shell/compare/v0.1.0...v0.2.0) (2026-07-14)


### Features

* **desktop:** add multi-tab terminal UI ([3e5b475](https://github.com/decode2/splice-shell/commit/3e5b47578577afd201370424170c2bb0bec63b3f))
* **desktop:** add resource safety and release automation ([d33101b](https://github.com/decode2/splice-shell/commit/d33101bf89126a8f7d66647c45ae3b947a08d418))
* **desktop:** copy the terminal selection to the clipboard ([6cc8db1](https://github.com/decode2/splice-shell/commit/6cc8db1c9d8489bc7b357c0f2a8f8a206addd486))
* **desktop:** dim the title bar when the window loses focus ([9f307e6](https://github.com/decode2/splice-shell/commit/9f307e6f16c9044a3289b9b7b8ef786bf2043ab7))
* **desktop:** establish splice shell MVP ([602f1e0](https://github.com/decode2/splice-shell/commit/602f1e04b1beaac64591ff3376e2d156e91fb139))
* **desktop:** redesign the title bar and drop ConPTY telemetry ([edeb24d](https://github.com/decode2/splice-shell/commit/edeb24d55968755d79e0667f97a7f81442b55592))
* **desktop:** Warp-style custom title bar ([0b2120d](https://github.com/decode2/splice-shell/commit/0b2120d0ba397431a4b9f25315278bb36efd5e96))
* **pty:** add end-to-end credit-based backpressure to the output pipeline ([d55bf49](https://github.com/decode2/splice-shell/commit/d55bf49d7fac75ddc15e1127062e080466d49ab6))
* **pty:** key sessions by id for concurrent terminals (tabs slice 1) ([359050d](https://github.com/decode2/splice-shell/commit/359050dece0c273d901fc10e9ed9858c95e11238))
* **splice-pty:** kill the whole process tree via a Job Object ([a6eb528](https://github.com/decode2/splice-shell/commit/a6eb5280a25563f9a2d40765d9955ce41cd67f81))
* **ui:** surface stalled sessions in tab health ([089bed9](https://github.com/decode2/splice-shell/commit/089bed92d65b7bfbe9f535112dc3612519350c69))


### Bug Fixes

* **clipboard:** re-encode clipboard images to PNG for AI-CLI paste ([71a8c46](https://github.com/decode2/splice-shell/commit/71a8c467b12e85ab8f2a3ce5ee19d2231f25f931))
* **clipboard:** retry OpenClipboard on the image/DIB paste path ([68bc769](https://github.com/decode2/splice-shell/commit/68bc76940bc588cc4b7f3103152315e0c8626f70))
* **desktop:** defer terminal refit to rAF to silence ResizeObserver loop ([d62e127](https://github.com/decode2/splice-shell/commit/d62e127744d6b7e8b5f126b112e1de50e02e97ed))
* **desktop:** don't hold PtyState lock during blocking PTY calls ([4b9c386](https://github.com/decode2/splice-shell/commit/4b9c3868e40e1a2e0db86d7d8dff31bca01032fd))
* **desktop:** fill the terminal window and drop the vestigial scrollbar ([cc41666](https://github.com/decode2/splice-shell/commit/cc41666160b562d2303502ed249b4e43a80da3ed))
* **desktop:** harden terminal file links ([1da8ba1](https://github.com/decode2/splice-shell/commit/1da8ba1f6600f66415469c7a9b2795005ab38eeb))
* **desktop:** hold the cursor-show back during Codex animations ([921a0f3](https://github.com/decode2/splice-shell/commit/921a0f31fb330d4778e2945ed56433a81b775468))
* **desktop:** make Ctrl+V paste into the terminal ([e4e1867](https://github.com/decode2/splice-shell/commit/e4e18674ec83501f8cdc8b8fc66b0bcd5d72ca4d))
* **desktop:** render glyphs via WebGL to stop Nerd Font clipping ([e476cd7](https://github.com/decode2/splice-shell/commit/e476cd78fda257e40b3ef356e62dd4c99a616650))
* **desktop:** route Ctrl+C through the PTY interrupt path ([08ddf2d](https://github.com/decode2/splice-shell/commit/08ddf2d3ca70ecbd41e8ca2edda95ff6ae4e9b70))
* **desktop:** stop terminal flicker on Codex animations ([2295e4e](https://github.com/decode2/splice-shell/commit/2295e4e49e656c9636a21f3f8a4dd44d8cead2ed))
* **desktop:** surface resize IPC failures; document output-filter seam ([8c597db](https://github.com/decode2/splice-shell/commit/8c597db05dd10e47d2686de1608d5f77212d6185))
* get CI green — Ctrl+C never interrupted anything ([4ab6834](https://github.com/decode2/splice-shell/commit/4ab6834bce9b3907da88a066319eb51d68427d96))
* make backpressure stalls observable and reap orphaned sessions ([8f5cc21](https://github.com/decode2/splice-shell/commit/8f5cc21f53976ea7721f0ea18cb22c8756dc21f3))
* **pty:** credit-based flow control — bound the output pipeline end to end ([fb32c37](https://github.com/decode2/splice-shell/commit/fb32c378afb4f4f7fdf461a5a1133c6345c1b3f7))
* **pty:** enforce the credit/ack liveness invariant at compile time ([27b0029](https://github.com/decode2/splice-shell/commit/27b0029fbc377ef55e664f8f7165451839062f56))
* **pty:** make backpressure stalls observable and non-blocking ([2ef8fe6](https://github.com/decode2/splice-shell/commit/2ef8fe6db14966835d003295f290e4e84e564a35))
* **pty:** make Ctrl+C actually interrupt console children ([25ffd11](https://github.com/decode2/splice-shell/commit/25ffd11c92189b1fc5d405a32e3ac6117087ee55))
* retry OpenClipboard on image paste + correct a stale verify-report ([b9b12cf](https://github.com/decode2/splice-shell/commit/b9b12cf92069f587aba00f639cef8ab702cbdab4))
* **security:** reveal file links instead of launching them ([5f2f479](https://github.com/decode2/splice-shell/commit/5f2f479835deab0a16a6383844c5a2925d1c266a))
* **security:** set a restrictive webview CSP ([8c8339d](https://github.com/decode2/splice-shell/commit/8c8339d85e9deae980587259f0e410cac76fcf45))
* **splice-pty:** decode ConPTY output on UTF-8 boundaries ([dc3f174](https://github.com/decode2/splice-shell/commit/dc3f1740a9e6a53ec97b806c544d5880590bdabd))
* **splice-pty:** harden ConPTY lifecycle; make pty_read type honest ([ef03471](https://github.com/decode2/splice-shell/commit/ef034712db3bf06c33d34493c81eb0e94a0d333a))


### Performance Improvements

* **pty:** replace liveness poll with a pushed pty-exit event ([4507b0a](https://github.com/decode2/splice-shell/commit/4507b0abac6a1da8e653b9ef06c1b09593167242))
