# Windows build header

`include/tap-windows.h` is an unmodified copy from OpenVPN/tap-windows6,
commit `0cad8664c2a51832df61f2e1853b6da317d1c129`.

Source: https://github.com/OpenVPN/tap-windows6/blob/0cad8664c2a51832df61f2e1853b6da317d1c129/src/tap-windows.h

The upstream header offers MIT or GPL-2.0 licensing. OvpnLane uses the MIT
option; see COPYRIGHT.MIT. Only the public header is included, not a driver.
OpenVPN Core 3.11.7 includes it indirectly from its Windows hardware-address
helpers, even with an external packet tunnel.

Dot-source `scripts/windows-env.ps1` before a Windows build to add this include
directory and enable standard C++ exception unwinding under MSVC.
