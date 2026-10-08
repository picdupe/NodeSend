// Exercise the headers and libraries that previously failed to compile/link.
#include <windows.h>
#include <oleauto.h>
#include <stdio.h>

int main() {
    BSTR value = SysAllocString(L"NodeSend SDK check");
    if (!value) return 1;
    unsigned int length = SysStringLen(value);
    SysFreeString(value);
    printf("MSVC / Windows SDK OK (BSTR length: %u)\n", length);
    return length == 18 ? 0 : 2;
}
