/*
 * A pass-through "GDI+" for the BC web server's media thumbnails on Linux.
 *
 * The web server ships the Windows build of System.Drawing.Common, whose GDI+ P/Invokes live in
 * System.Private.Windows.GdiPlus (and GdiplusStartup in System.Private.Windows.Core). BC only uses System.Drawing
 * on image bytes it already has in the right size, mostly the thumbnails the service tier renders for media
 * fields (customer and item pictures, the role center's 70x70 and 360x0 images):
 *
 *   LogicalMediaThumbnail.LoadResourceCore:  Image.FromStream(stream)
 *   LogicalImage.Image setter:               image.RawFormat, image.Save(stream, image.RawFormat)
 *   LogicalImage.Image getter:               image.Clone()
 *   ImageControl.TryLoadImage, WebImageHelper: Image.FromStream(stream).RawFormat (content type)
 *   LogicalImage (embedded images):          new Bitmap(stream)
 *
 * None of these looks at pixels. The bytes go in, the format is read, and the same bytes come back out with the
 * same encoder. This library implements exactly the GDI+ flat API those calls reach, and nothing else:
 *
 *   GdiplusStartup                                  -> token 1
 *   GdipLoadImageFromStream[ICM], GdipCreateBitmapFromStream[ICM]
 *                                                   -> read the COM IStream to its end, keep the bytes, detect
 *                                                      PNG, JPEG, GIF, BMP, TIFF or ICO by signature
 *   GdipImageForceValidation                        -> Ok
 *   GdipGetImageType                                -> ImageTypeBitmap
 *   GdipGetImageRawFormat                           -> the detected format's GUID (ImageFormat.Png, ...)
 *   GdipImageGetFrameDimensionsCount                -> 0 (GIFs are not treated as animated; their bytes are
 *                                                      saved unchanged either way)
 *   GdipCloneImage, GdipDisposeImage                -> copy, free
 *   GdipGetImageEncodersSize, GdipGetImageEncoders  -> GDI+'s built-in BMP, JPEG, GIF, TIFF and PNG encoders
 *   GdipSaveImageToStream                           -> write the original bytes if the encoder matches the
 *                                                      image's format, else NotImplemented
 *
 * Anything that would need real decoding fails with a GDI+ error status (UnknownImageFormat, NotImplemented),
 * which System.Drawing turns into an exception that BC's ImageResourceCache logs. That covers EMF/WMF, format
 * conversion and drawing, e.g. WebImageHelper's icon-to-PNG conversion for .ico pictures. Every other GDI+
 * function stays missing, so it throws EntryPointNotFoundException instead of misbehaving.
 *
 * The hosting startup (../LinuxHostingStartup) maps "gdiplus.dll" to this file for System.Private.Windows.GdiPlus
 * and System.Private.Windows.Core only, through NativeLibrary.SetDllImportResolver.
 *
 * The IStream* comes from System.Drawing's ComManagedStream (ComWrappers), whose vtable entries are
 * [UnmanagedCallersOnly] functions with the platform calling convention, so C calls them directly.
 *
 * Build: gcc -shared -fPIC -O2 -Wall -o libbclinux-gdiplus.so gdiplusshim.c
 */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef int32_t Status;
enum { Ok = 0, GenericError = 1, InvalidParameter = 2, OutOfMemory = 3, InsufficientBuffer = 5,
       NotImplemented = 6, UnknownImageFormat = 13 };
enum { ImageTypeBitmap = 1 };

typedef struct { uint32_t d1; uint16_t d2, d3; uint8_t d4[8]; } GUID;

typedef int32_t HRESULT;
typedef struct IStream IStream;
typedef struct {
    HRESULT (*QueryInterface)(IStream *, const GUID *, void **);
    uint32_t (*AddRef)(IStream *);
    uint32_t (*Release)(IStream *);
    HRESULT (*Read)(IStream *, void *, uint32_t, uint32_t *);
    HRESULT (*Write)(IStream *, const void *, uint32_t, uint32_t *);
} IStreamVtbl;   /* only the first five IStream entries are used */
struct IStream { const IStreamVtbl *vtbl; };

/* GDI+ image format GUIDs {b96b3caX-0728-11d3-9d7b-0000f81ef32e} and encoder CLSIDs
 * {557cf40X-1a04-11d3-9a73-0000f81ef32e}, as in gdiplusimaging.h and GdipGetImageEncoders on Windows. */
#define FORMAT_GUID(n) { 0xb96b3ca0u + (n), 0x0728, 0x11d3, { 0x9d, 0x7b, 0x00, 0x00, 0xf8, 0x1e, 0xf3, 0x2e } }
#define ENCODER_CLSID(n) { 0x557cf400u + (n), 0x1a04, 0x11d3, { 0x9a, 0x73, 0x00, 0x00, 0xf8, 0x1e, 0xf3, 0x2e } }

enum { FMT_BMP, FMT_JPEG, FMT_GIF, FMT_TIFF, FMT_PNG, FMT_ICON, FMT_COUNT };
static const GUID format_guid[FMT_COUNT] = {
    [FMT_BMP] = FORMAT_GUID(0xb), [FMT_JPEG] = FORMAT_GUID(0xe), [FMT_GIF] = FORMAT_GUID(0x10),
    [FMT_TIFF] = FORMAT_GUID(0x11), [FMT_PNG] = FORMAT_GUID(0xf), [FMT_ICON] = FORMAT_GUID(0x15),
};

/* Windows.Win32.Graphics.GdiPlus.ImageCodecInfo (gdiplusimagecodec.h), 104 bytes on 64-bit. */
typedef struct {
    GUID Clsid, FormatID;
    const uint16_t *CodecName, *DllName, *FormatDescription, *FilenameExtension, *MimeType;
    uint32_t Flags, Version, SigCount, SigSize;
    const uint8_t *SigPattern, *SigMask;
} ImageCodecInfo;

#define U16(s) ((const uint16_t *)u##s)
static const struct { int format; GUID clsid; const uint16_t *name, *desc, *ext, *mime; } encoders[] = {
    { FMT_BMP, ENCODER_CLSID(0), U16("Built-in BMP Codec"), U16("BMP"), U16("*.BMP;*.DIB;*.RLE"), U16("image/bmp") },
    { FMT_JPEG, ENCODER_CLSID(1), U16("Built-in JPEG Codec"), U16("JPEG"), U16("*.JPG;*.JPEG;*.JPE;*.JFIF"), U16("image/jpeg") },
    { FMT_GIF, ENCODER_CLSID(2), U16("Built-in GIF Codec"), U16("GIF"), U16("*.GIF"), U16("image/gif") },
    { FMT_TIFF, ENCODER_CLSID(5), U16("Built-in TIFF Codec"), U16("TIFF"), U16("*.TIF;*.TIFF"), U16("image/tiff") },
    { FMT_PNG, ENCODER_CLSID(6), U16("Built-in PNG Codec"), U16("PNG"), U16("*.PNG"), U16("image/png") },
};
#define ENCODER_COUNT (sizeof encoders / sizeof encoders[0])

#define IMAGE_MAGIC 0x42434c47u /* "BCLG" */
typedef struct {
    uint32_t magic;
    int format;
    size_t size;
    uint8_t data[];
} Image;

static Image *as_image(void *p)
{
    Image *img = p;
    return img && img->magic == IMAGE_MAGIC ? img : NULL;
}

static int detect_format(const uint8_t *d, size_t n)
{
    if (n >= 8 && !memcmp(d, "\x89PNG\r\n\x1a\n", 8)) return FMT_PNG;
    if (n >= 3 && d[0] == 0xFF && d[1] == 0xD8 && d[2] == 0xFF) return FMT_JPEG;
    if (n >= 6 && (!memcmp(d, "GIF87a", 6) || !memcmp(d, "GIF89a", 6))) return FMT_GIF;
    if (n >= 14 && d[0] == 'B' && d[1] == 'M') return FMT_BMP;
    if (n >= 4 && (!memcmp(d, "II*\0", 4) || !memcmp(d, "MM\0*", 4))) return FMT_TIFF;
    if (n >= 6 && d[0] == 0 && d[1] == 0 && (d[2] == 1 || d[2] == 2) && d[3] == 0) return FMT_ICON;
    return -1;
}

/* Reads the stream from its current position to the end, like GDI+ does. */
static Status load(IStream *stream, void **out)
{
    if (!stream || !stream->vtbl || !out) return InvalidParameter;
    *out = NULL;
    size_t cap = 64 * 1024, len = 0;
    uint8_t *buf = malloc(cap);
    if (!buf) return OutOfMemory;
    for (;;) {
        if (len == cap) {
            if (cap >= (size_t)256 << 20) { free(buf); return OutOfMemory; }
            uint8_t *bigger = realloc(buf, cap * 2);
            if (!bigger) { free(buf); return OutOfMemory; }
            buf = bigger;
            cap *= 2;
        }
        uint32_t got = 0;
        HRESULT hr = stream->vtbl->Read(stream, buf + len, (uint32_t)(cap - len), &got);
        if (hr < 0) { free(buf); return GenericError; }
        if (got == 0) break;
        len += got;
    }
    int format = detect_format(buf, len);
    if (format < 0) { free(buf); return UnknownImageFormat; }
    Image *img = malloc(sizeof *img + len);
    if (!img) { free(buf); return OutOfMemory; }
    img->magic = IMAGE_MAGIC;
    img->format = format;
    img->size = len;
    memcpy(img->data, buf, len);
    free(buf);
    *out = img;
    return Ok;
}

Status GdiplusStartup(uintptr_t *token, const void *input, void *output)
{
    (void)input; (void)output;
    if (!token) return InvalidParameter;
    *token = 1;
    return Ok;
}

Status GdipLoadImageFromStream(IStream *stream, void **image) { return load(stream, image); }
Status GdipLoadImageFromStreamICM(IStream *stream, void **image) { return load(stream, image); }
Status GdipCreateBitmapFromStream(IStream *stream, void **bitmap) { return load(stream, bitmap); }
Status GdipCreateBitmapFromStreamICM(IStream *stream, void **bitmap) { return load(stream, bitmap); }

Status GdipImageForceValidation(void *image) { return as_image(image) ? Ok : InvalidParameter; }

Status GdipGetImageType(void *image, int32_t *type)
{
    if (!as_image(image) || !type) return InvalidParameter;
    *type = ImageTypeBitmap;
    return Ok;
}

Status GdipGetImageRawFormat(void *image, GUID *format)
{
    Image *img = as_image(image);
    if (!img || !format) return InvalidParameter;
    *format = format_guid[img->format];
    return Ok;
}

Status GdipImageGetFrameDimensionsCount(void *image, uint32_t *count)
{
    if (!as_image(image) || !count) return InvalidParameter;
    *count = 0;
    return Ok;
}

Status GdipCloneImage(void *image, void **clone)
{
    Image *img = as_image(image);
    if (!img || !clone) return InvalidParameter;
    Image *copy = malloc(sizeof *img + img->size);
    if (!copy) return OutOfMemory;
    memcpy(copy, img, sizeof *img + img->size);
    *clone = copy;
    return Ok;
}

Status GdipDisposeImage(void *image)
{
    Image *img = as_image(image);
    if (!img) return InvalidParameter;
    img->magic = 0;
    free(img);
    return Ok;
}

Status GdipGetImageEncodersSize(uint32_t *count, uint32_t *size)
{
    if (!count || !size) return InvalidParameter;
    *count = ENCODER_COUNT;
    *size = ENCODER_COUNT * sizeof(ImageCodecInfo);
    return Ok;
}

Status GdipGetImageEncoders(uint32_t count, uint32_t size, ImageCodecInfo *out)
{
    if (!out || count != ENCODER_COUNT) return InvalidParameter;
    if (size < ENCODER_COUNT * sizeof(ImageCodecInfo)) return InsufficientBuffer;
    for (size_t i = 0; i < ENCODER_COUNT; i++) {
        ImageCodecInfo *c = &out[i];
        memset(c, 0, sizeof *c);
        c->Clsid = encoders[i].clsid;
        c->FormatID = format_guid[encoders[i].format];
        c->CodecName = encoders[i].name;
        c->FormatDescription = encoders[i].desc;
        c->FilenameExtension = encoders[i].ext;
        c->MimeType = encoders[i].mime;
        c->Flags = 0x00010007;   /* ImageCodecFlagsEncoder | Decoder | SupportBitmap | Builtin */
        c->Version = 1;
    }
    return Ok;
}

Status GdipSaveImageToStream(void *image, IStream *stream, const GUID *clsid, const void *params)
{
    (void)params;
    Image *img = as_image(image);
    if (!img || !stream || !stream->vtbl || !clsid) return InvalidParameter;
    for (size_t i = 0; i < ENCODER_COUNT; i++) {
        if (memcmp(&encoders[i].clsid, clsid, sizeof *clsid)) continue;
        if (encoders[i].format != img->format) return NotImplemented;   /* would need a real transcoder */
        size_t done = 0;
        while (done < img->size) {
            size_t chunk = img->size - done > 0x40000000 ? 0x40000000 : img->size - done;
            uint32_t written = 0;
            HRESULT hr = stream->vtbl->Write(stream, img->data + done, (uint32_t)chunk, &written);
            if (hr < 0 || written == 0) return GenericError;
            done += written;
        }
        return Ok;
    }
    return InvalidParameter;   /* not an encoder CLSID */
}
