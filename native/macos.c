#include <CoreFoundation/CoreFoundation.h>
#include <CoreServices/CoreServices.h>
#include <dispatch/dispatch.h>
#include <mach/mach.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/stat.h>
/* One translation unit lets clang inline Oil Find's small matching helpers. */
#include "../vendor/oil-find/coilfind.c"

typedef struct { uint32_t start, len; } lumi_span;

/* Names are already Unicode-normalized/lowercased in Rust. No candidate cap. */
size_t lumi_search(const uint8_t *blob, const lumi_span *names,
                   const lumi_span *full, const lumi_span *initials,
                   const uint8_t *kinds, uint32_t count,
                   const uint8_t *needle, size_t len, int ascii,
                   uint32_t *ids, int32_t *scores) {
    size_t found = 0;
    for (uint32_t i = 0; i < count; i++) {
        if (kinds[i] & 128) continue;
        lumi_span n = names[i];
        int quality = sift_contains(blob + n.start, n.len, needle, len, 1)
            ? sift_match_quality(blob + n.start, n.len, needle, len, 1) : 0;
        int score = quality * 1000;
        if (!quality && ascii && full[i].len) {
            n = full[i];
            quality = sift_contains(blob + n.start, n.len, needle, len, 1)
                ? sift_match_quality(blob + n.start, n.len, needle, len, 1) : 0;
            score = quality * 100 + 100;
            if (!quality) {
                n = initials[i];
                quality = sift_contains(blob + n.start, n.len, needle, len, 1)
                    ? sift_match_quality(blob + n.start, n.len, needle, len, 1) : 0;
                score = quality * 100;
            }
        }
        if (quality) {
            ids[found] = i;
            scores[found++] = score + ((kinds[i] & 3) == 1 ? 10 : 0);
        }
    }
    return found;
}

int lumi_transliterate(const uint8_t *input, size_t len, char *output, size_t cap) {
    CFStringRef source = CFStringCreateWithBytes(NULL, input, len, kCFStringEncodingUTF8, false);
    if (!source) return 0;
    CFMutableStringRef text = CFStringCreateMutableCopy(NULL, 0, source);
    CFRelease(source);
    CFStringTransform(text, NULL, kCFStringTransformMandarinLatin, false);
    CFStringTransform(text, NULL, kCFStringTransformStripDiacritics, false);
    int ok = CFStringGetCString(text, output, cap, kCFStringEncodingUTF8);
    CFRelease(text);
    return ok;
}

typedef void (*lumi_event_callback)(void *, const char *, uint32_t, uint64_t);
typedef struct {
    FSEventStreamRef stream;
    dispatch_queue_t queue;
    lumi_event_callback callback;
    void *context;
} lumi_watcher;

static void events(ConstFSEventStreamRef stream, void *info, size_t count,
                   void *paths, const FSEventStreamEventFlags *flags,
                   const FSEventStreamEventId *ids) {
    (void)stream;
    lumi_watcher *w = info;
    for (size_t i = 0; i < count; i++) w->callback(w->context, ((char **)paths)[i], flags[i], ids[i]);
}

void *lumi_watch_start(const char *root, uint64_t since, lumi_event_callback cb, void *context) {
    lumi_watcher *w = calloc(1, sizeof(*w));
    if (!w) return NULL;
    w->callback = cb; w->context = context;
    CFStringRef path = CFStringCreateWithCString(NULL, root, kCFStringEncodingUTF8);
    if (!path) { free(w); return NULL; }
    CFArrayRef paths = CFArrayCreate(NULL, (const void **)&path, 1, &kCFTypeArrayCallBacks);
    FSEventStreamContext ctx = {0, w, NULL, NULL, NULL};
    w->stream = FSEventStreamCreate(NULL, events, &ctx, paths, since, 0.10,
        kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagNoDefer | kFSEventStreamCreateFlagWatchRoot);
    CFRelease(paths); CFRelease(path);
    if (!w->stream) { free(w); return NULL; }
    w->queue = dispatch_queue_create("org.luminator.events", DISPATCH_QUEUE_SERIAL);
    FSEventStreamSetDispatchQueue(w->stream, w->queue);
    if (!FSEventStreamStart(w->stream)) {
        FSEventStreamInvalidate(w->stream); FSEventStreamRelease(w->stream);
        dispatch_release(w->queue); free(w); return NULL;
    }
    return w;
}

static void drained(void *unused) { (void)unused; }
void lumi_watch_stop(void *watcher) {
    lumi_watcher *w = watcher;
    FSEventStreamStop(w->stream);
    FSEventStreamInvalidate(w->stream);
    dispatch_sync_f(w->queue, NULL, drained);
    FSEventStreamRelease(w->stream);
    dispatch_release(w->queue);
    free(w);
}
uint64_t lumi_event_id(void) { return FSEventsGetCurrentEventId(); }
int lumi_volume_uuid(int32_t dev, char *out, size_t capacity) {
    CFUUIDRef uuid = FSEventsCopyUUIDForDevice(dev);
    if (!uuid) return 0;
    CFStringRef text = CFUUIDCreateString(NULL, uuid);
    int ok = CFStringGetCString(text, out, capacity, kCFStringEncodingUTF8);
    CFRelease(text); CFRelease(uuid); return ok;
}
int lumi_dataless(uint32_t flags) { return (flags & SF_DATALESS) != 0; }
uint64_t lumi_footprint(void) {
    task_vm_info_data_t info;
    mach_msg_type_number_t count = TASK_VM_INFO_COUNT;
    if (task_info(mach_task_self(), TASK_VM_INFO, (task_info_t)&info, &count) != KERN_SUCCESS) return 0;
    return info.phys_footprint;
}
uint64_t lumi_footprint_peak(void) {
    task_vm_info_data_t info;
    mach_msg_type_number_t count = TASK_VM_INFO_COUNT;
    if (task_info(mach_task_self(), TASK_VM_INFO, (task_info_t)&info, &count) != KERN_SUCCESS
        || count < TASK_VM_INFO_REV3_COUNT) return 0;
    return info.ledger_phys_footprint_peak > 0 ? (uint64_t)info.ledger_phys_footprint_peak : 0;
}
