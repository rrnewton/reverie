# reverie-loader

A dependency-free `no_std` library for validating Hermit's fixed-address loader
and runtime images and describing the loader's initialization stages.

The optional `exec-stop` and `stage1a-probe` features of `reverie-inguest` use
these types for host launch mechanics and their tests. Publishing this library
does not enable those features or qualify experimental native execution.

Licensed under BSD-2-Clause, like the Reverie workspace.
