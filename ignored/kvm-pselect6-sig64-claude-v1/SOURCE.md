Repository: https://github.com/rrnewton/reverie
Base: be09e5100bca6dad77aede0349de9a5c92990854
Head: 1fdadb7940dc232d07c1e36494f0f102b74f3140
Tree: f676d5d8dcfb266e92e2e656661ff5e08f73f787
Pull request: https://github.com/rrnewton/reverie/pull/606

BASE-TO-HEAD.patch is the complete five-file diff. candidate/ contains every
changed file at the exact head plus vm.rs and Cargo manifests as context.
before/ contains every pre-existing changed file at the exact base; the new
integration test has no base version. All packet inputs are hashed before the
review and checked again after it.
