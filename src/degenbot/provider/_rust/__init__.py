"""ADR-013 barrier: the single binding site for the Rust provider pyclasses.

The provider submodule trees (``sync``, ``async_provider``,
``offline_provider``) construct the Rust pyclasses, but ``degenbot._ffi``
is imported only here. One binding site keeps the compiled seam out of the
submodule import graphs and gives the package ``__init__`` a cycle-free
re-export, so submodules no longer need to defer the binding into their
function bodies.
"""

from degenbot._ffi import provider as _ffi_provider

RustAlloyProvider = _ffi_provider.AlloyProvider
RustAsyncAlloyProvider = _ffi_provider.AsyncAlloyProvider
