#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/hid/IOHIDManager.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static uint64_t values;

enum { MAX_ELEMENTS = 64 };

struct ElementStats {
    uint32_t usage_page;
    uint32_t usage;
    int64_t logical_min;
    int64_t logical_max;
    int64_t observed_min;
    int64_t observed_max;
    uint64_t count;
};

static struct ElementStats elements[MAX_ELEMENTS];
static size_t element_count;

enum { MAX_REPORT_BYTES = 128 };

static uint64_t reports;
static uint8_t byte_min[MAX_REPORT_BYTES];
static uint8_t byte_max[MAX_REPORT_BYTES];
static uint64_t byte_changes[MAX_REPORT_BYTES];
static uint8_t first_report[MAX_REPORT_BYTES];
static size_t first_report_length;
static uint8_t last_report[MAX_REPORT_BYTES];
static size_t last_report_length;

static void report_changed(
    void *context,
    IOReturn result,
    void *sender,
    IOHIDReportType report_type,
    uint32_t report_id,
    uint8_t *report,
    CFIndex report_length
) {
    (void)context;
    (void)result;
    (void)sender;
    (void)report_type;
    (void)report_id;
    size_t length = (size_t)report_length < MAX_REPORT_BYTES ? (size_t)report_length : MAX_REPORT_BYTES;
    if (reports == 0) {
        first_report_length = length;
        for (size_t index = 0; index < length; index++) {
            first_report[index] = report[index];
            byte_min[index] = report[index];
            byte_max[index] = report[index];
        }
    } else {
        for (size_t index = 0; index < length; index++) {
            byte_min[index] = byte_min[index] < report[index] ? byte_min[index] : report[index];
            byte_max[index] = byte_max[index] > report[index] ? byte_max[index] : report[index];
            byte_changes[index] += report[index] != first_report[index];
        }
    }
    last_report_length = length;
    for (size_t index = 0; index < length; index++)
        last_report[index] = report[index];
    reports++;
}

static void value_changed(void *context, IOReturn result, void *sender, IOHIDValueRef value) {
    (void)context;
    (void)result;
    (void)sender;
    IOHIDElementRef element = IOHIDValueGetElement(value);
    uint32_t usage_page = IOHIDElementGetUsagePage(element);
    uint32_t usage = IOHIDElementGetUsage(element);
    int64_t observed = IOHIDValueGetIntegerValue(value);
    for (size_t index = 0; index < element_count; index++) {
        struct ElementStats *entry = &elements[index];
        if (entry->usage_page == usage_page && entry->usage == usage) {
            entry->observed_min = entry->observed_min < observed ? entry->observed_min : observed;
            entry->observed_max = entry->observed_max > observed ? entry->observed_max : observed;
            entry->count++;
            values++;
            return;
        }
    }
    if (element_count < MAX_ELEMENTS) {
        struct ElementStats *entry = &elements[element_count++];
        *entry = (struct ElementStats) {
            usage_page,
            usage,
            IOHIDElementGetLogicalMin(element),
            IOHIDElementGetLogicalMax(element),
            observed,
            observed,
            1,
        };
    }
    values++;
}
static CFNumberRef number(int value) {
    return CFNumberCreate(kCFAllocatorDefault, kCFNumberIntType, &value);
}

int main(int argc, char **argv) {
    double seconds = argc == 2 ? strtod(argv[1], NULL) : 15.0;
    if (seconds <= 0.0) {
        fputs("usage: ds4-hid-probe [positive-seconds]\n", stderr);
        return 2;
    }

    IOHIDManagerRef manager = IOHIDManagerCreate(kCFAllocatorDefault, kIOHIDOptionsTypeNone);
    int vendor = 0x054c;
    int product = 0x09cc;
    CFNumberRef vendor_number = number(vendor);
    IOHIDManagerRegisterInputReportCallback(manager, report_changed, NULL);
    CFNumberRef product_number = number(product);
    const void *keys[] = { CFSTR("VendorID"), CFSTR("ProductID") };
    const void *matching_values[] = { vendor_number, product_number };
    CFDictionaryRef matching = CFDictionaryCreate(
        kCFAllocatorDefault, keys, matching_values, 2,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    IOHIDManagerSetDeviceMatching(manager, matching);
    IOHIDManagerRegisterInputValueCallback(manager, value_changed, NULL);
    IOHIDManagerScheduleWithRunLoop(manager, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
    if (IOHIDManagerOpen(manager, kIOHIDOptionsTypeNone) != kIOReturnSuccess) {
        fputs("IOHID manager open refused\n", stderr);
        return 3;
    }

    CFSetRef devices = IOHIDManagerCopyDevices(manager);
    printf("matched_devices %ld\n", devices ? CFSetGetCount(devices) : 0L);
    CFAbsoluteTime deadline = CFAbsoluteTimeGetCurrent() + seconds;
    while (CFAbsoluteTimeGetCurrent() < deadline)
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, false);

    printf("input_values %llu\n", values);
    if (devices) CFRelease(devices);
    for (size_t index = 0; index < element_count; index++) {
        const struct ElementStats *entry = &elements[index];
        printf(
            "usage_page=0x%04x usage=0x%04x logical=[%lld,%lld] observed=[%lld,%lld] changes=%llu\n",
            entry->usage_page,
            entry->usage,
            entry->logical_min,
            entry->logical_max,
            entry->observed_min,
            entry->observed_max,
            entry->count);
    }
    printf("input_reports %llu first_length %zu\n", reports, first_report_length);
    for (size_t index = 0; index < first_report_length; index++) {
        if (byte_min[index] != byte_max[index]) {
            printf(
                "byte=%zu neutral=%u observed=[%u,%u] changed_reports=%llu\n",
                index, first_report[index], byte_min[index], byte_max[index], byte_changes[index]);
        }
    }
    CFRelease(matching);
    printf("first_report");
    for (size_t index = 0; index < first_report_length; index++)
        printf(" %02x", first_report[index]);
    printf("\nlast_report");
    for (size_t index = 0; index < last_report_length; index++)
        printf(" %02x", last_report[index]);
    printf("\n");
    CFRelease(vendor_number);
    CFRelease(product_number);
    IOHIDManagerClose(manager, kIOHIDOptionsTypeNone);
    CFRelease(manager);
    return values == 0 ? 4 : 0;
}
