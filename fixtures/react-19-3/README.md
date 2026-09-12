# React 19.3 conformance

This private Yarn PnP workspace resolves real matching React packages without changing the root
19.2.8 baseline. `crates/wake_test/tests/react19_conformance.rs` creates temporary suites here
and runs the same adapter assertions in DOM and system Chromium. No third-party source is vendored.

Registry provenance (2026-09-12), locked with canonical checksums in the root `yarn.lock`:

| Package | Version | npm integrity |
| --- | --- | --- |
| react | 19.3.0 | `sha512-E8LUcbtBWt20bbl2YoHfx4ZDBdxVTfOKtCZn9cDSJ4l6/nuoApcpIBcj47t2wZoVX8g2ZHuMHbiShgCR1T5Sog==` |
| react-dom | 19.3.0 | `sha512-JDk8dgif51OjFoDE70+OT9ICyYr+69HlmihNwp1+Nsfbna3t5sIiCa9ZJktDmQ4/1b/rn26hIAR2uYXDMr5r0Q==` |

Both packages are MIT licensed and report gitHead `1d34f91dfde6bba84d08b683aaba164c7194dacb`.
Tarballs: https://registry.npmjs.org/react/-/react-19.3.0.tgz and
https://registry.npmjs.org/react-dom/-/react-dom-19.3.0.tgz.
