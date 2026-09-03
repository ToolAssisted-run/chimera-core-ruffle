/* Rust std keeps its gcc personality + gimli backtrace objects even under
 * panic=immediate-abort; they reference the libgcc _Unwind_* API but are never
 * executed (no unwinding ever happens). The host libgcc_eh is small-model and
 * glibc-linked, so it cannot be used at the large-model guest base. Supply
 * inert stubs instead. If one is ever actually called, that is a bug: trap. */
#include <stdlib.h>
#define STUB(name) void name(void) { abort(); }
STUB(_Unwind_GetIP) STUB(_Unwind_GetIPInfo) STUB(_Unwind_SetIP)
STUB(_Unwind_GetGR) STUB(_Unwind_SetGR) STUB(_Unwind_GetLanguageSpecificData)
STUB(_Unwind_GetRegionStart) STUB(_Unwind_GetTextRelBase) STUB(_Unwind_GetDataRelBase)
STUB(_Unwind_GetCFA) STUB(_Unwind_Backtrace) STUB(_Unwind_FindEnclosingFunction)
STUB(_Unwind_Resume) STUB(_Unwind_RaiseException) STUB(_Unwind_DeleteException)
STUB(_Unwind_Resume_or_Rethrow) STUB(_Unwind_ForcedUnwind)
