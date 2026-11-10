/*
 * Linux implementation of the five user32 functions that Microsoft.Dynamics.Framework.UI P/Invokes
 * (Microsoft.Dynamics.Framework.UI.NativeMethods), emulating the Windows "US" keyboard layout
 * (KLID 00000409, kbdus.dll):
 *
 *   GetKeyboardLayoutNameW(buf)                          -> "00000409"
 *   LoadKeyboardLayoutW(klid, KLF_ACTIVATE)              -> HKL 0x04090409
 *   GetKeyState(vk)                                      -> 0 (no key down, Caps Lock off)
 *   MapVirtualKeyExW(code, type, hkl)                    -> kbdus scan code <-> virtual key tables
 *   ToUnicodeEx(vk, sc, keystate, buf, cch, flags, hkl)  -> the character kbdus produces
 *
 * KeyboardMapper uses them to build the server's keyboard map: for every scan code 1..127 it asks for the
 * virtual key, then asks ToUnicodeEx for the character of each key with Caps Lock off and on. The result names
 * the keys in action shortcuts ("Ctrl+Alt+N", "Shift+F9", ...), for every action of every page, starting with the
 * role center. Without this library that fails with DllNotFoundException: user32.dll.
 *
 * KeyboardMapper's ClearKeyboardBuffer loops until ToUnicodeEx(VK_DECIMAL) returns 1, so the character results
 * have to be right, not just non-failing. On a Windows web server the layout is the IIS worker's, which on a BC
 * server or container is US. GetKeyState returns 0 like it does for a process without keyboard input. There are
 * no dead keys on the US layout, so ToUnicodeEx never returns -1.
 *
 * Only Microsoft.Dynamics.Framework.UI gets this library: the hosting startup (../LinuxHostingStartup) maps
 * "user32.dll" to it for that one assembly through NativeLibrary.SetDllImportResolver.
 *
 * WCHAR is 16-bit (the managed side marshals char[] as UTF-16), BOOL is a 32-bit int, HKL is pointer-sized.
 *
 * Build: gcc -shared -fPIC -O2 -Wall -o libbclinux-user32.so user32shim.c
 */
#include <stddef.h>
#include <stdint.h>

#define HKL_US ((intptr_t)0x04090409)

enum {
    VK_CANCEL = 0x03, VK_BACK = 0x08, VK_TAB = 0x09, VK_CLEAR = 0x0C, VK_RETURN = 0x0D,
    VK_SHIFT = 0x10, VK_CONTROL = 0x11, VK_MENU = 0x12, VK_PAUSE = 0x13, VK_CAPITAL = 0x14,
    VK_ESCAPE = 0x1B, VK_SPACE = 0x20, VK_PRIOR = 0x21, VK_NEXT = 0x22, VK_END = 0x23, VK_HOME = 0x24,
    VK_LEFT = 0x25, VK_UP = 0x26, VK_RIGHT = 0x27, VK_DOWN = 0x28, VK_SNAPSHOT = 0x2C, VK_INSERT = 0x2D,
    VK_DELETE = 0x2E, VK_HELP = 0x2F, VK_LWIN = 0x5B, VK_RWIN = 0x5C, VK_APPS = 0x5D, VK_SLEEP = 0x5F,
    VK_NUMPAD0 = 0x60, VK_MULTIPLY = 0x6A, VK_ADD = 0x6B, VK_SUBTRACT = 0x6D, VK_DECIMAL = 0x6E,
    VK_DIVIDE = 0x6F, VK_F1 = 0x70, VK_F11 = 0x7A, VK_F13 = 0x7C, VK_F24 = 0x87, VK_NUMLOCK = 0x90,
    VK_SCROLL = 0x91, VK_LSHIFT = 0xA0, VK_RSHIFT = 0xA1, VK_LCONTROL = 0xA2, VK_RCONTROL = 0xA3,
    VK_LMENU = 0xA4, VK_RMENU = 0xA5,
    VK_OEM_1 = 0xBA, VK_OEM_PLUS = 0xBB, VK_OEM_COMMA = 0xBC, VK_OEM_MINUS = 0xBD, VK_OEM_PERIOD = 0xBE,
    VK_OEM_2 = 0xBF, VK_OEM_3 = 0xC0, VK_ABNT_C1 = 0xC1, VK_ABNT_C2 = 0xC2, VK_OEM_4 = 0xDB,
    VK_OEM_5 = 0xDC, VK_OEM_6 = 0xDD, VK_OEM_7 = 0xDE, VK_OEM_102 = 0xE2, VK_OEM_RESET = 0xE9,
    VK_OEM_JUMP = 0xEA, VK_OEM_PA1 = 0xEB, VK_OEM_PA2 = 0xEC, VK_OEM_PA3 = 0xED, VK_OEM_WSCTRL = 0xEE,
    VK_OEM_FINISH = 0xF1, VK_OEM_AUTO = 0xF3, VK_OEM_BACKTAB = 0xF5, VK_EREOF = 0xF9, VK_ZOOM = 0xFB,
};

/* kbdus scan code (set 1, no prefix) -> virtual key, as in kbd.h's T00..T7F. 0 = no key. Numpad keys give
 * the Num Lock off keys (VK_HOME, ...), as MapVirtualKey does. */
static const uint8_t sc_to_vk[128] = {
    [0x01] = VK_ESCAPE,
    [0x02] = '1', [0x03] = '2', [0x04] = '3', [0x05] = '4', [0x06] = '5',
    [0x07] = '6', [0x08] = '7', [0x09] = '8', [0x0A] = '9', [0x0B] = '0',
    [0x0C] = VK_OEM_MINUS, [0x0D] = VK_OEM_PLUS, [0x0E] = VK_BACK, [0x0F] = VK_TAB,
    [0x10] = 'Q', [0x11] = 'W', [0x12] = 'E', [0x13] = 'R', [0x14] = 'T',
    [0x15] = 'Y', [0x16] = 'U', [0x17] = 'I', [0x18] = 'O', [0x19] = 'P',
    [0x1A] = VK_OEM_4, [0x1B] = VK_OEM_6, [0x1C] = VK_RETURN, [0x1D] = VK_LCONTROL,
    [0x1E] = 'A', [0x1F] = 'S', [0x20] = 'D', [0x21] = 'F', [0x22] = 'G',
    [0x23] = 'H', [0x24] = 'J', [0x25] = 'K', [0x26] = 'L',
    [0x27] = VK_OEM_1, [0x28] = VK_OEM_7, [0x29] = VK_OEM_3, [0x2A] = VK_LSHIFT, [0x2B] = VK_OEM_5,
    [0x2C] = 'Z', [0x2D] = 'X', [0x2E] = 'C', [0x2F] = 'V', [0x30] = 'B', [0x31] = 'N', [0x32] = 'M',
    [0x33] = VK_OEM_COMMA, [0x34] = VK_OEM_PERIOD, [0x35] = VK_OEM_2, [0x36] = VK_RSHIFT,
    [0x37] = VK_MULTIPLY, [0x38] = VK_LMENU, [0x39] = VK_SPACE, [0x3A] = VK_CAPITAL,
    [0x3B] = VK_F1, [0x3C] = VK_F1 + 1, [0x3D] = VK_F1 + 2, [0x3E] = VK_F1 + 3, [0x3F] = VK_F1 + 4,
    [0x40] = VK_F1 + 5, [0x41] = VK_F1 + 6, [0x42] = VK_F1 + 7, [0x43] = VK_F1 + 8, [0x44] = VK_F1 + 9,
    [0x45] = VK_NUMLOCK, [0x46] = VK_SCROLL,
    [0x47] = VK_HOME, [0x48] = VK_UP, [0x49] = VK_PRIOR, [0x4A] = VK_SUBTRACT,
    [0x4B] = VK_LEFT, [0x4C] = VK_CLEAR, [0x4D] = VK_RIGHT, [0x4E] = VK_ADD,
    [0x4F] = VK_END, [0x50] = VK_DOWN, [0x51] = VK_NEXT, [0x52] = VK_INSERT, [0x53] = VK_DELETE,
    [0x54] = VK_SNAPSHOT, [0x56] = VK_OEM_102, [0x57] = VK_F11, [0x58] = VK_F11 + 1,
    [0x59] = VK_CLEAR, [0x5A] = VK_OEM_WSCTRL, [0x5B] = VK_OEM_FINISH, [0x5C] = VK_OEM_JUMP,
    [0x5D] = VK_EREOF, [0x5E] = VK_OEM_BACKTAB, [0x5F] = VK_OEM_AUTO, [0x62] = VK_ZOOM, [0x63] = VK_HELP,
    [0x64] = VK_F13, [0x65] = VK_F13 + 1, [0x66] = VK_F13 + 2, [0x67] = VK_F13 + 3, [0x68] = VK_F13 + 4,
    [0x69] = VK_F13 + 5, [0x6A] = VK_F13 + 6, [0x6B] = VK_F13 + 7, [0x6C] = VK_F13 + 8, [0x6D] = VK_F13 + 9,
    [0x6E] = VK_F13 + 10, [0x6F] = VK_OEM_PA3, [0x71] = VK_OEM_RESET, [0x73] = VK_ABNT_C1,
    [0x76] = VK_F24, [0x7B] = VK_OEM_PA1, [0x7C] = VK_TAB, [0x7E] = VK_ABNT_C2, [0x7F] = VK_OEM_PA2,
};

/* Keys that only exist with the E0 prefix (kbdus aE0VscToVk), for VK -> scan code. */
static const struct { uint8_t sc, vk; } e0_keys[] = {
    {0x1C, VK_RETURN}, {0x1D, VK_RCONTROL}, {0x35, VK_DIVIDE}, {0x37, VK_SNAPSHOT}, {0x38, VK_RMENU},
    {0x46, VK_CANCEL}, {0x47, VK_HOME}, {0x48, VK_UP}, {0x49, VK_PRIOR}, {0x4B, VK_LEFT}, {0x4D, VK_RIGHT},
    {0x4F, VK_END}, {0x50, VK_DOWN}, {0x51, VK_NEXT}, {0x52, VK_INSERT}, {0x53, VK_DELETE},
    {0x5B, VK_LWIN}, {0x5C, VK_RWIN}, {0x5D, VK_APPS}, {0x5F, VK_SLEEP},
};

/* Num Lock on: numpad scan codes -> VK_NUMPAD0..9, VK_DECIMAL (only used for VK -> scan code). */
static const struct { uint8_t sc, vk; } numpad_keys[] = {
    {0x52, VK_NUMPAD0}, {0x4F, VK_NUMPAD0 + 1}, {0x50, VK_NUMPAD0 + 2}, {0x51, VK_NUMPAD0 + 3},
    {0x4B, VK_NUMPAD0 + 4}, {0x4C, VK_NUMPAD0 + 5}, {0x4D, VK_NUMPAD0 + 6}, {0x47, VK_NUMPAD0 + 7},
    {0x48, VK_NUMPAD0 + 8}, {0x49, VK_NUMPAD0 + 9}, {0x53, VK_DECIMAL},
};

/* kbdus character tables (aVkToWch1..4). Columns: none, Shift, Ctrl, Shift+Ctrl. NONE = no character. */
#define NONE 0xFFFF
static const struct { uint8_t vk; uint16_t ch[4]; } chars[] = {
    {VK_OEM_3, {'`', '~', NONE, NONE}},
    {'1', {'1', '!', NONE, NONE}},
    {'2', {'2', '@', NONE, 0x0000}},
    {'3', {'3', '#', NONE, NONE}},
    {'4', {'4', '$', NONE, NONE}},
    {'5', {'5', '%', NONE, NONE}},
    {'6', {'6', '^', NONE, 0x001E}},
    {'7', {'7', '&', NONE, NONE}},
    {'8', {'8', '*', NONE, NONE}},
    {'9', {'9', '(', NONE, NONE}},
    {'0', {'0', ')', NONE, NONE}},
    {VK_OEM_MINUS, {'-', '_', NONE, 0x001F}},
    {VK_OEM_PLUS, {'=', '+', NONE, NONE}},
    {VK_OEM_4, {'[', '{', 0x001B, NONE}},
    {VK_OEM_6, {']', '}', 0x001D, NONE}},
    {VK_OEM_5, {'\\', '|', 0x001C, NONE}},
    {VK_OEM_102, {'\\', '|', 0x001C, NONE}},
    {VK_OEM_1, {';', ':', NONE, NONE}},
    {VK_OEM_7, {'\'', '"', NONE, NONE}},
    {VK_OEM_COMMA, {',', '<', NONE, NONE}},
    {VK_OEM_PERIOD, {'.', '>', NONE, NONE}},
    {VK_OEM_2, {'/', '?', NONE, NONE}},
    {VK_BACK, {'\b', '\b', 0x007F, NONE}},
    {VK_ESCAPE, {0x001B, 0x001B, 0x001B, NONE}},
    {VK_RETURN, {'\r', '\r', '\n', NONE}},
    {VK_SPACE, {' ', ' ', ' ', NONE}},
    {VK_CANCEL, {0x0003, 0x0003, 0x0003, NONE}},
    {VK_TAB, {'\t', '\t', NONE, NONE}},
    {VK_DECIMAL, {'.', '.', NONE, NONE}},
    {VK_ADD, {'+', '+', NONE, NONE}},
    {VK_SUBTRACT, {'-', '-', NONE, NONE}},
    {VK_MULTIPLY, {'*', '*', NONE, NONE}},
    {VK_DIVIDE, {'/', '/', NONE, NONE}},
};

static int vk_is_letter(uint32_t vk) { return vk >= 'A' && vk <= 'Z'; }

/* The character of vk without modifiers, or NONE. */
static uint32_t base_char(uint32_t vk)
{
    if (vk_is_letter(vk)) return vk - 'A' + 'a';
    if (vk >= VK_NUMPAD0 && vk <= VK_NUMPAD0 + 9) return '0' + (vk - VK_NUMPAD0);
    for (size_t i = 0; i < sizeof chars / sizeof chars[0]; i++)
        if (chars[i].vk == vk) return chars[i].ch[0];
    return NONE;
}

static uint32_t generic_vk(uint32_t vk)
{
    switch (vk) {
    case VK_LSHIFT: case VK_RSHIFT: return VK_SHIFT;
    case VK_LCONTROL: case VK_RCONTROL: return VK_CONTROL;
    case VK_LMENU: case VK_RMENU: return VK_MENU;
    default: return vk;
    }
}

/* VK -> scan code: the first scan code in the kbdus tables that produces it. */
static uint32_t vk_to_sc(uint32_t vk, int with_prefix)
{
    switch (vk) {
    case VK_SHIFT: vk = VK_LSHIFT; break;
    case VK_CONTROL: vk = VK_LCONTROL; break;
    case VK_MENU: vk = VK_LMENU; break;
    case VK_PAUSE: return 0x45;
    }
    for (uint32_t sc = 1; sc < 128; sc++)
        if (sc_to_vk[sc] == vk) return sc;
    for (size_t i = 0; i < sizeof numpad_keys / sizeof numpad_keys[0]; i++)
        if (numpad_keys[i].vk == vk) return numpad_keys[i].sc;
    for (size_t i = 0; i < sizeof e0_keys / sizeof e0_keys[0]; i++)
        if (e0_keys[i].vk == vk) return with_prefix ? 0xE000u | e0_keys[i].sc : e0_keys[i].sc;
    return 0;
}

static uint32_t sc_to_vk_ex(uint32_t code)
{
    if ((code & 0xFF00) == 0xE000) {
        for (size_t i = 0; i < sizeof e0_keys / sizeof e0_keys[0]; i++)
            if (e0_keys[i].sc == (code & 0xFF)) return e0_keys[i].vk;
        return 0;
    }
    return code < 128 ? sc_to_vk[code] : 0;
}

/* MAPVK_VK_TO_VSC 0, MAPVK_VSC_TO_VK 1, MAPVK_VK_TO_CHAR 2, MAPVK_VSC_TO_VK_EX 3, MAPVK_VK_TO_VSC_EX 4 */
uint32_t MapVirtualKeyExW(uint32_t code, uint32_t type, intptr_t hkl)
{
    (void)hkl;
    switch (type) {
    case 0: return vk_to_sc(code, 0);
    case 1: return code < 128 ? generic_vk(sc_to_vk[code]) : 0;
    case 2: {
        uint32_t c = base_char(code);
        if (c == NONE) return 0;
        return (c >= 'a' && c <= 'z') ? c - 'a' + 'A' : c;   /* MapVirtualKey reports letters in upper case */
    }
    case 3: return sc_to_vk_ex(code);
    case 4: return vk_to_sc(code, 1);
    default: return 0;
    }
}

int32_t ToUnicodeEx(uint32_t vk, uint32_t sc, const uint8_t *key_state, uint16_t *buf, int32_t cch,
                    uint32_t flags, intptr_t hkl)
{
    (void)flags; (void)hkl;
    if ((sc & 0x8000) || !buf || cch < 1) return 0;   /* key up, or nowhere to write */
    static const uint8_t no_keys[256];
    const uint8_t *ks = key_state ? key_state : no_keys;
    int shift = ((ks[VK_SHIFT] | ks[VK_LSHIFT] | ks[VK_RSHIFT]) & 0x80) != 0;
    int ctrl = ((ks[VK_CONTROL] | ks[VK_LCONTROL] | ks[VK_RCONTROL]) & 0x80) != 0;
    int alt = ((ks[VK_MENU] | ks[VK_LMENU] | ks[VK_RMENU]) & 0x80) != 0;
    int caps = (ks[VK_CAPITAL] & 0x01) != 0;
    if (alt) return 0;   /* kbdus has no Alt or AltGr (Ctrl+Alt) characters */

    uint32_t c = NONE;
    if (vk_is_letter(vk)) {
        if (ctrl) c = shift ? NONE : vk - 'A' + 1;   /* Ctrl+A = 0x01 ... Ctrl+Z = 0x1A */
        else c = (shift != caps) ? vk : vk - 'A' + 'a';
    } else if (vk >= VK_NUMPAD0 && vk <= VK_NUMPAD0 + 9) {
        if (!shift && !ctrl) c = '0' + (vk - VK_NUMPAD0);
    } else {
        for (size_t i = 0; i < sizeof chars / sizeof chars[0]; i++)
            if (chars[i].vk == vk) { c = chars[i].ch[shift + 2 * ctrl]; break; }
    }
    if (c == NONE) return 0;
    buf[0] = (uint16_t)c;
    if (cch > 1) buf[1] = 0;
    return 1;
}

int16_t GetKeyState(int32_t vk)
{
    (void)vk;
    return 0;
}

intptr_t LoadKeyboardLayoutW(const uint16_t *klid, uint32_t flags)
{
    (void)klid; (void)flags;   /* Windows falls back to the default layout for a KLID it does not have */
    return HKL_US;
}

/* buf holds KL_NAMELENGTH (9) WCHARs. */
int32_t GetKeyboardLayoutNameW(uint16_t *buf)
{
    static const char name[] = "00000409";
    if (!buf) return 0;
    for (size_t i = 0; i < sizeof name; i++) buf[i] = (uint16_t)name[i];
    return 1;
}
