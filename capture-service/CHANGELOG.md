# Changelog

## 0.1.0 (2026-09-26)


### ⚠ BREAKING CHANGES

* **capture:** enabled capture, Run and Attention integrations require a capture companion advertising interface 1. Update the companion before reloading the plugin; core chat remains independent.
* **push:** deploy receiver and Android together. Registration uses PUT /v1/attention/installation with fid; the token endpoint is removed. Registry schema 3 retires old targets until Android registers its FID.
* **config:** optional editor and service capabilities default off. Explicit choices preserve core chat and retained Run obligations.
* **attention:** AttentionStore::new requires an explicit RunStore.
* **rust:** Rust capture request APIs now borrow their inputs.
* **pairing:** v2 pairing QR payloads now use compact field names; receiver and Android app must be upgraded together.
* **capture:** pairing registry state uses schema version 2 and Android upload state now persists an explicit receiver owner.
* **capture:** pairing QR v1, serve --bind, pair --url, and capture.receiver_url are removed. Configure one private receiver profile before pairing.
* **capture:** add durable speech intake

### Features

* **android:** add opt-in Attention notifications ([37de434](https://github.com/euri10/louiselm/commit/37de434f6f4410712fcce4cda0644bced1c278c1))
* **attention:** add private mobile inbox ([4a8d8b9](https://github.com/euri10/louiselm/commit/4a8d8b905ea18b02e61657f9feaa781995714827))
* **attention:** add skill posture alerts ([60183d7](https://github.com/euri10/louiselm/commit/60183d7a3c726efdddd006a0e8176d1fe26cdc3e))
* **attention:** wire broker state into Neovim ([ed737fe](https://github.com/euri10/louiselm/commit/ed737fe8c04c23a06b0716c402d598d3f5e76eb0))
* **broker:** add lifecycle recovery foundations ([2619dd7](https://github.com/euri10/louiselm/commit/2619dd7da65558eb13198cae45418fe55731b5f3))
* **broker:** persist skill admission requests ([2a94e15](https://github.com/euri10/louiselm/commit/2a94e15b9001fba47abf92704bc17a6d4df743e6))
* **broker:** propagate skill quarantine ([fc2fdea](https://github.com/euri10/louiselm/commit/fc2fdeab3f262ed655370a6db37ba8e2d9b188ab))
* **capture-service:** expose Generator reservations on the run CLI ([2963cb1](https://github.com/euri10/louiselm/commit/2963cb148a0297a4d3fa656b2c4e099a690b1b81))
* **capture:** add durable speech intake ([ba19b8a](https://github.com/euri10/louiselm/commit/ba19b8acf8781a27beea1bafdc01eb3fad22de89))
* **capture:** bind queues to receiver identity ([efe6a15](https://github.com/euri10/louiselm/commit/efe6a1558c131bdd403faf7f8c93cdf53fdd0ffa))
* **capture:** configure private receiver ([9440eaa](https://github.com/euri10/louiselm/commit/9440eaadc8c37c6ed911b6e2ad7545072cc6bd5d))
* **capture:** deliver optional FCM alerts ([5e60d65](https://github.com/euri10/louiselm/commit/5e60d6590f18dc02a8788744e00550e52125778b))
* **capture:** release and check companion ([98b6f05](https://github.com/euri10/louiselm/commit/98b6f05b2590aa9a81aca276d4eb64427da08f01))
* **config:** require optional feature opt-ins ([e68abfc](https://github.com/euri10/louiselm/commit/e68abfc94e7239613c94fff737c604b277f396ec))
* **push:** migrate addressing to FIDs ([3491658](https://github.com/euri10/louiselm/commit/3491658e55d25274efadd66d5c21412280dd93e2))
* **service:** add Run Park command ([e538f53](https://github.com/euri10/louiselm/commit/e538f53ec8ec26a68f5e86c6f46b71d14e51fbc1))
* **service:** persist expired Run cleanup ([39f64f2](https://github.com/euri10/louiselm/commit/39f64f2217d872cbc0616276029b50a1fb4a42a6))
* **service:** reap expired Beads claims ([9460ece](https://github.com/euri10/louiselm/commit/9460ece3d9bcc538c5a06694bd45486ecc4c4de8))
* **ui:** add cold Park command ([71a6ae3](https://github.com/euri10/louiselm/commit/71a6ae32b8035dd1806ba53d8c07e13dfe690b7a))
* **workflow:** broker bounded Beads work ([849fc67](https://github.com/euri10/louiselm/commit/849fc67714c31b84a9fc6da10d009c7420c7408f))
* **workflow:** enable durable Park reaping ([731b381](https://github.com/euri10/louiselm/commit/731b3811ee0f0db3eedc4834267a3adc0a338cce))
* **workflow:** execute bounded outcomes ([c01f9a8](https://github.com/euri10/louiselm/commit/c01f9a82853b331c26b66049f1024dcced61d6a3))
* **workflow:** implement operator raise/resume for budget Parks ([93db877](https://github.com/euri10/louiselm/commit/93db877402661a649b307df39439d719b6f3a46c))
* **workflow:** persist cold Park resume metadata ([34ff3da](https://github.com/euri10/louiselm/commit/34ff3da94b19d0a3174d351d463dab22568cac5b))
* **workflow:** persist Run generation budget ([5476127](https://github.com/euri10/louiselm/commit/5476127c848737fc8fcfe4c05bc5c3067ea25aec))
* **workflow:** restore Park Run state ([64523ac](https://github.com/euri10/louiselm/commit/64523ac5d0861b22115ce6d460535804330a966c))
* **workflow:** resume durable Parks ([ffeef23](https://github.com/euri10/louiselm/commit/ffeef237a0dc543c54bd455dfe156616ed5e04d3))
* **workflow:** stream Run invalidations ([c65ca9f](https://github.com/euri10/louiselm/commit/c65ca9f2df7be2a865be6c526bd93986fb40694a))


### Bug Fixes

* **attention:** clear durable session focus ([0aea35b](https://github.com/euri10/louiselm/commit/0aea35bfafddfcbd253145098b0bfc0518925f0a))
* **attention:** count paste as activity ([5180db8](https://github.com/euri10/louiselm/commit/5180db84a408e8559b9bddd1aed45ce44d1abdfe))
* **attention:** ignore streamed output for idle ([eef5f88](https://github.com/euri10/louiselm/commit/eef5f88a8145be137e57e364c76fbe1fb43b6263))
* **attention:** isolate broker producer access ([1d1f08c](https://github.com/euri10/louiselm/commit/1d1f08cdd028986e6af21c8a00190c09102f1408))
* **attention:** preserve Park recovery state ([f8de2ed](https://github.com/euri10/louiselm/commit/f8de2ed7639083d51c916840c942ed8912a26180))
* **attention:** reconcile stale Run Park alerts ([f7a2163](https://github.com/euri10/louiselm/commit/f7a2163f0c3a1c04c2a4ce9d416071eb3916b7e2))
* **capture-service:** allow AF_UNIX in the reference systemd unit ([27458e3](https://github.com/euri10/louiselm/commit/27458e3de00633cff8199ed76d97ca115ea6f5c9))
* **capture-service:** make the durable Run reaper actually reap ([c819b27](https://github.com/euri10/louiselm/commit/c819b27e2570eb48904737917736674fbc4ad5c6))
* **capture-service:** reuse mutation identity on ambiguous generate retry ([efc0039](https://github.com/euri10/louiselm/commit/efc0039527124d08c9a319327d1c629397b2e46e))
* **capture:** preserve broker identities ([256f0c9](https://github.com/euri10/louiselm/commit/256f0c93f368d45e550b1de0aaffc8cb13758197))
* **capture:** shrink pairing QR ([456031b](https://github.com/euri10/louiselm/commit/456031b1779665fae48a417b1826791c5fa697d0))
* **capture:** tolerate absent broker policy ([6719ebd](https://github.com/euri10/louiselm/commit/6719ebdc65914b0864fa701f068901be7cfa4cdd))
* **pairing:** reduce QR density ([9e194da](https://github.com/euri10/louiselm/commit/9e194da1f3b8c1e946c2d616565dd84560d7db54))
* **park:** show newest Parks first ([dcf87d0](https://github.com/euri10/louiselm/commit/dcf87d0ffb13def4934c6167ec063e76814e16a4))
* **service:** protect Run records ([b0e5605](https://github.com/euri10/louiselm/commit/b0e5605137b3d0ff23ac7e4cdd8c1c6917b54cbb))


### Miscellaneous Chores

* **rust:** enforce strict quality gates ([71fe994](https://github.com/euri10/louiselm/commit/71fe99467fac607c043e5f73bd4d30969c65c851))
