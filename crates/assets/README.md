# assets

- Owns: built-in themes, cursor assets, icons, and shell integration scripts as packaged assets.
- Must not import: terminal parser internals, transport implementations, platform window backends, renderer hot paths.
- Layer: visual overlay and config portability.
- Tests required: asset manifest integrity, theme schema compatibility, cursor asset loading, script packaging, and license tracking.
