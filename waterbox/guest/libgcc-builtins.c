/* libgcc builtins the guest needs but cannot borrow from the host: the host's
 * libgcc is small-model and glibc-linked, so it cannot live at the large-model
 * guest base. Mesa's bitscan.h calls __builtin_popcountll, which gcc lowers to
 * a libgcc call when it cannot assume the POPCNT instruction - and we compile
 * for a baseline CPU on purpose, because choosing a code path per host CPU is
 * exactly what a deterministic core must not do. Plain C, no ISA assumptions. */
int __popcountdi2(unsigned long long x)
{
	/* SWAR popcount; same answer on every machine */
	x = x - ((x >> 1) & 0x5555555555555555ULL);
	x = (x & 0x3333333333333333ULL) + ((x >> 2) & 0x3333333333333333ULL);
	x = (x + (x >> 4)) & 0x0f0f0f0f0f0f0f0fULL;
	return (int)((x * 0x0101010101010101ULL) >> 56);
}

int __popcountsi2(unsigned int x)
{
	return __popcountdi2((unsigned long long)x);
}
