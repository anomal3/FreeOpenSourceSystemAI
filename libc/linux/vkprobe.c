/* Проба Vulkan (18.11.2026, NVK поверх GSP-RM, веха M6): маленький клиент
 * загрузчика Vulkan из пакета Proton (libvulkan.so.1 1.4.309). Собирается на
 * glibc Debian (`debian::build_glibc_probe`), кладётся в `bin/` пакета Proton
 * с загрузчиком `/opt/proton/glibc/ld.so` (как vulkaninfo); запуск —
 * `fossai-proton --run=vkprobe [имя-устройства]`.
 *
 * Заголовков Vulkan в sysroot нет, поэтому нужные структуры описаны здесь
 * (vulkan_core.h 1.4, x86-64); библиотека — через dlopen, функции — через
 * vkGetInstanceProcAddr.
 *
 *   1. экземпляр, список устройств: имя, тип, драйвер (VkPhysicalDeviceDriverProperties);
 *   2. выбор: устройство, имя которого содержит argv[1], иначе первое
 *      дискретное, иначе первое;
 *   3. логическое устройство с одной очередью (графика или вычисления, иначе
 *      передача), пустой командный буфер + забор, ожидание забора;
 *   4. два буфера в памяти HOST_VISIBLE|HOST_COHERENT: vkCmdFillBuffer
 *      0x12345678 в первый, vkCmdCopyBuffer в второй, чтение и сверка.
 *
 * Вывод: `vkprobe: ok ...` / `vkprobe: FAILED ...`; итог `vkprobe: 0 failed`
 * или `vkprobe: N checks failed`.
 */

#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef int32_t VkResult;
typedef void *VkInstance;
typedef void *VkPhysicalDevice;
typedef void *VkDevice;
typedef void *VkQueue;
typedef void *VkCommandBuffer;
typedef uint64_t VkCommandPool, VkFence, VkBuffer, VkDeviceMemory;
typedef void (*PFN_void)(void);
typedef PFN_void (*PFN_vkGetInstanceProcAddr)(VkInstance, const char *);

enum {
    VK_STRUCTURE_TYPE_APPLICATION_INFO = 0,
    VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO = 1,
    VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO = 2,
    VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO = 3,
    VK_STRUCTURE_TYPE_SUBMIT_INFO = 4,
    VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO = 5,
    VK_STRUCTURE_TYPE_FENCE_CREATE_INFO = 8,
    VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO = 12,
    VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO = 39,
    VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO = 40,
    VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO = 42,
    VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2 = 1000059001,
    VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES = 1000196000,
};
enum { VK_QUEUE_GRAPHICS_BIT = 1, VK_QUEUE_COMPUTE_BIT = 2, VK_QUEUE_TRANSFER_BIT = 4 };
enum { VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT = 2, VK_MEMORY_PROPERTY_HOST_COHERENT_BIT = 4 };
enum { VK_BUFFER_USAGE_TRANSFER_SRC_BIT = 1, VK_BUFFER_USAGE_TRANSFER_DST_BIT = 2 };
enum { VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT = 1 };
#define VK_API_VERSION_1_1 ((1u << 22) | (1u << 12))
#define VK_WHOLE_SIZE (~0ull)

typedef struct { uint32_t sType; const void *pNext; const char *pApplicationName; uint32_t applicationVersion; const char *pEngineName; uint32_t engineVersion, apiVersion; } VkApplicationInfo;
typedef struct { uint32_t sType; const void *pNext; uint32_t flags; const VkApplicationInfo *pApplicationInfo; uint32_t enabledLayerCount; const char *const *ppEnabledLayerNames; uint32_t enabledExtensionCount; const char *const *ppEnabledExtensionNames; } VkInstanceCreateInfo;
/* VkPhysicalDeviceProperties: нужна только голова (до deviceName); остальное — место. */
typedef struct { uint32_t apiVersion, driverVersion, vendorID, deviceID, deviceType; char deviceName[256]; uint8_t rest[1024]; } VkPhysicalDeviceProperties;
typedef struct { uint32_t sType; void *pNext; VkPhysicalDeviceProperties properties; } VkPhysicalDeviceProperties2;
typedef struct { uint32_t sType; void *pNext; uint32_t driverID; char driverName[256]; char driverInfo[256]; uint8_t conformanceVersion[4]; } VkPhysicalDeviceDriverProperties;
typedef struct { uint32_t queueFlags, queueCount, timestampValidBits, granularity[3]; } VkQueueFamilyProperties;
typedef struct { uint32_t propertyFlags, heapIndex; } VkMemoryType;
typedef struct { uint64_t size; uint32_t flags; } VkMemoryHeap;
typedef struct { uint32_t memoryTypeCount; VkMemoryType memoryTypes[32]; uint32_t memoryHeapCount; VkMemoryHeap memoryHeaps[16]; } VkPhysicalDeviceMemoryProperties;
typedef struct { uint32_t sType; const void *pNext; uint32_t flags, queueFamilyIndex, queueCount; const float *pQueuePriorities; } VkDeviceQueueCreateInfo;
typedef struct { uint32_t sType; const void *pNext; uint32_t flags, queueCreateInfoCount; const VkDeviceQueueCreateInfo *pQueueCreateInfos; uint32_t enabledLayerCount; const char *const *ppEnabledLayerNames; uint32_t enabledExtensionCount; const char *const *ppEnabledExtensionNames; const void *pEnabledFeatures; } VkDeviceCreateInfo;
typedef struct { uint32_t sType; const void *pNext; uint32_t flags, queueFamilyIndex; } VkCommandPoolCreateInfo;
typedef struct { uint32_t sType; const void *pNext; VkCommandPool commandPool; uint32_t level, commandBufferCount; } VkCommandBufferAllocateInfo;
typedef struct { uint32_t sType; const void *pNext; uint32_t flags; const void *pInheritanceInfo; } VkCommandBufferBeginInfo;
typedef struct { uint32_t sType; const void *pNext; uint32_t flags; } VkFenceCreateInfo;
typedef struct { uint32_t sType; const void *pNext; uint32_t waitSemaphoreCount; const void *pWaitSemaphores; const uint32_t *pWaitDstStageMask; uint32_t commandBufferCount; const VkCommandBuffer *pCommandBuffers; uint32_t signalSemaphoreCount; const void *pSignalSemaphores; } VkSubmitInfo;
typedef struct { uint32_t sType; const void *pNext; uint32_t flags; uint64_t size; uint32_t usage, sharingMode, queueFamilyIndexCount; const uint32_t *pQueueFamilyIndices; } VkBufferCreateInfo;
typedef struct { uint64_t size, alignment; uint32_t memoryTypeBits; } VkMemoryRequirements;
typedef struct { uint32_t sType; const void *pNext; uint64_t allocationSize; uint32_t memoryTypeIndex; } VkMemoryAllocateInfo;
typedef struct { uint64_t srcOffset, dstOffset, size; } VkBufferCopy;

_Static_assert(sizeof(VkInstanceCreateInfo) == 64, "VkInstanceCreateInfo");
_Static_assert(sizeof(VkDeviceCreateInfo) == 72, "VkDeviceCreateInfo");
_Static_assert(sizeof(VkSubmitInfo) == 72, "VkSubmitInfo");
_Static_assert(sizeof(VkBufferCreateInfo) == 56, "VkBufferCreateInfo");
_Static_assert(sizeof(VkPhysicalDeviceMemoryProperties) == 520, "VkPhysicalDeviceMemoryProperties");
_Static_assert(__builtin_offsetof(VkPhysicalDeviceProperties2, properties) == 16, "properties2");
_Static_assert(__builtin_offsetof(VkPhysicalDeviceDriverProperties, driverName) == 20, "driverName");

static int failed;
static void check(int good, const char *what, long got) {
    if (good)
        printf("vkprobe: ok %s\n", what);
    else {
        printf("vkprobe: FAILED %s (got %ld)\n", what, got);
        failed++;
    }
    fflush(stdout);
}
static int finish(void) {
    if (failed)
        printf("vkprobe: %d checks failed\n", failed);
    else
        printf("vkprobe: 0 failed\n");
    fflush(stdout);
    return failed;
}

static PFN_vkGetInstanceProcAddr gipa;
static VkInstance inst;
#define FN(type, name) __typeof__(type) name = (__typeof__(type))gipa(inst, #name)

static const char *type_name(uint32_t t) {
    static const char *const n[5] = {"other", "integrated", "discrete", "virtual", "cpu"};
    return t < 5 ? n[t] : "?";
}

int main(int argc, char **argv) {
    printf("vkprobe: start\n");
    /* Без настроек fossai-proton: оба ICD пакета (NVK первым). */
    if (!getenv("VK_DRIVER_FILES") && !getenv("VK_ICD_FILENAMES"))
        setenv("VK_DRIVER_FILES",
               "/opt/proton/share/vulkan/icd.d/nouveau_icd.x86_64.json:/opt/proton/share/vulkan/icd.d/lvp_icd.x86_64.json", 1);
    void *lib = dlopen("libvulkan.so.1", RTLD_NOW | RTLD_LOCAL);
    check(lib != NULL, "dlopen libvulkan.so.1", 0);
    if (!lib) {
        printf("vkprobe: note %s\n", dlerror());
        return finish();
    }
    gipa = (PFN_vkGetInstanceProcAddr)dlsym(lib, "vkGetInstanceProcAddr");
    check(gipa != NULL, "vkGetInstanceProcAddr", 0);
    if (!gipa) return finish();

    FN(VkResult (*)(const VkInstanceCreateInfo *, const void *, VkInstance *), vkCreateInstance);
    VkApplicationInfo app = {VK_STRUCTURE_TYPE_APPLICATION_INFO, NULL, "vkprobe", 1, "fossai", 1, VK_API_VERSION_1_1};
    VkInstanceCreateInfo ici = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, NULL, 0, &app, 0, NULL, 0, NULL};
    VkResult r = vkCreateInstance(&ici, NULL, &inst);
    check(r == 0, "vkCreateInstance (Vulkan 1.1)", r);
    if (r) return finish();

    FN(VkResult (*)(VkInstance, uint32_t *, VkPhysicalDevice *), vkEnumeratePhysicalDevices);
    FN(void (*)(VkPhysicalDevice, VkPhysicalDeviceProperties2 *), vkGetPhysicalDeviceProperties2);
    FN(void (*)(VkPhysicalDevice, uint32_t *, VkQueueFamilyProperties *), vkGetPhysicalDeviceQueueFamilyProperties);
    FN(void (*)(VkPhysicalDevice, VkPhysicalDeviceMemoryProperties *), vkGetPhysicalDeviceMemoryProperties);
    FN(VkResult (*)(VkPhysicalDevice, const VkDeviceCreateInfo *, const void *, VkDevice *), vkCreateDevice);
    FN(void (*)(VkDevice, uint32_t, uint32_t, VkQueue *), vkGetDeviceQueue);
    FN(VkResult (*)(VkDevice, const VkCommandPoolCreateInfo *, const void *, VkCommandPool *), vkCreateCommandPool);
    FN(VkResult (*)(VkDevice, const VkCommandBufferAllocateInfo *, VkCommandBuffer *), vkAllocateCommandBuffers);
    FN(VkResult (*)(VkCommandBuffer, const VkCommandBufferBeginInfo *), vkBeginCommandBuffer);
    FN(VkResult (*)(VkCommandBuffer), vkEndCommandBuffer);
    FN(VkResult (*)(VkCommandBuffer, uint32_t), vkResetCommandBuffer);
    FN(VkResult (*)(VkDevice, const VkFenceCreateInfo *, const void *, VkFence *), vkCreateFence);
    FN(VkResult (*)(VkDevice, uint32_t, const VkFence *), vkResetFences);
    FN(VkResult (*)(VkQueue, uint32_t, const VkSubmitInfo *, VkFence), vkQueueSubmit);
    FN(VkResult (*)(VkDevice, uint32_t, const VkFence *, uint32_t, uint64_t), vkWaitForFences);
    FN(VkResult (*)(VkDevice, const VkBufferCreateInfo *, const void *, VkBuffer *), vkCreateBuffer);
    FN(void (*)(VkDevice, VkBuffer, VkMemoryRequirements *), vkGetBufferMemoryRequirements);
    FN(VkResult (*)(VkDevice, const VkMemoryAllocateInfo *, const void *, VkDeviceMemory *), vkAllocateMemory);
    FN(VkResult (*)(VkDevice, VkBuffer, VkDeviceMemory, uint64_t), vkBindBufferMemory);
    FN(VkResult (*)(VkDevice, VkDeviceMemory, uint64_t, uint64_t, uint32_t, void **), vkMapMemory);
    FN(void (*)(VkCommandBuffer, VkBuffer, uint64_t, uint64_t, uint32_t), vkCmdFillBuffer);
    FN(void (*)(VkCommandBuffer, VkBuffer, VkBuffer, uint32_t, const VkBufferCopy *), vkCmdCopyBuffer);
    FN(void (*)(VkCommandBuffer, uint32_t, uint32_t, uint32_t, uint32_t, const void *, uint32_t, const void *, uint32_t, const void *), vkCmdPipelineBarrier);
    FN(VkResult (*)(VkDevice), vkDeviceWaitIdle);
    FN(void (*)(VkDevice, const void *), vkDestroyDevice);
    FN(void (*)(VkInstance, const void *), vkDestroyInstance);

    uint32_t count = 0;
    r = vkEnumeratePhysicalDevices(inst, &count, NULL);
    check(r == 0 && count > 0, "vkEnumeratePhysicalDevices finds a device", r ? r : (long)count);
    if (r || !count) return finish();
    VkPhysicalDevice devs[8];
    if (count > 8) count = 8;
    vkEnumeratePhysicalDevices(inst, &count, devs);
    int pick = -1, discrete = -1;
    for (uint32_t i = 0; i < count; i++) {
        VkPhysicalDeviceDriverProperties drv;
        memset(&drv, 0, sizeof drv);
        drv.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES;
        VkPhysicalDeviceProperties2 p2;
        memset(&p2, 0, sizeof p2);
        p2.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2;
        p2.pNext = &drv;
        vkGetPhysicalDeviceProperties2(devs[i], &p2);
        VkPhysicalDeviceProperties *p = &p2.properties;
        printf("vkprobe: device %u '%s' type %s vendor 0x%04x device 0x%04x api %u.%u.%u driver '%s' '%s'\n", i, p->deviceName,
               type_name(p->deviceType), p->vendorID, p->deviceID, p->apiVersion >> 22, (p->apiVersion >> 12) & 0x3ff,
               p->apiVersion & 0xfff, drv.driverName, drv.driverInfo);
        if (argc > 1 && pick < 0 && strstr(p->deviceName, argv[1])) pick = (int)i;
        if (discrete < 0 && p->deviceType == 2) discrete = (int)i;
    }
    fflush(stdout);
    if (argc > 1) check(pick >= 0, "a device with the requested name is listed", 0);
    if (pick < 0) pick = discrete >= 0 ? discrete : 0;
    if (argc > 1 && failed) return finish();
    VkPhysicalDevice pd = devs[pick];
    printf("vkprobe: using device %d\n", pick);

    VkQueueFamilyProperties qf[16];
    uint32_t nqf = 16;
    vkGetPhysicalDeviceQueueFamilyProperties(pd, &nqf, qf);
    int family = -1;
    for (uint32_t i = 0; i < nqf && family < 0; i++)
        if (qf[i].queueFlags & (VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT)) family = (int)i;
    for (uint32_t i = 0; i < nqf && family < 0; i++)
        if (qf[i].queueFlags & VK_QUEUE_TRANSFER_BIT) family = (int)i;
    check(family >= 0, "a queue family that can transfer", nqf);
    if (family < 0) return finish();
    printf("vkprobe: queue family %d flags 0x%x\n", family, qf[family].queueFlags);

    float prio = 1.0f;
    VkDeviceQueueCreateInfo qci = {VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, NULL, 0, (uint32_t)family, 1, &prio};
    VkDeviceCreateInfo dci = {VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, NULL, 0, 1, &qci, 0, NULL, 0, NULL, NULL};
    VkDevice dev;
    r = vkCreateDevice(pd, &dci, NULL, &dev);
    check(r == 0, "vkCreateDevice", r);
    if (r) return finish();
    VkQueue q;
    vkGetDeviceQueue(dev, (uint32_t)family, 0, &q);

    VkCommandPoolCreateInfo pci = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, NULL, 2 /* RESET_COMMAND_BUFFER */, (uint32_t)family};
    VkCommandPool pool;
    r = vkCreateCommandPool(dev, &pci, NULL, &pool);
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, NULL, pool, 0, 1};
    VkCommandBuffer cb;
    VkResult r2 = r ? r : vkAllocateCommandBuffers(dev, &cai, &cb);
    check(r == 0 && r2 == 0, "command pool and buffer", r ? r : r2);
    if (r || r2) return finish();

    /* Пустой командный буфер и забор. */
    VkCommandBufferBeginInfo bi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, NULL, VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT, NULL};
    vkBeginCommandBuffer(cb, &bi);
    r = vkEndCommandBuffer(cb);
    VkFenceCreateInfo fci = {VK_STRUCTURE_TYPE_FENCE_CREATE_INFO, NULL, 0};
    VkFence fence;
    vkCreateFence(dev, &fci, NULL, &fence);
    VkSubmitInfo si = {VK_STRUCTURE_TYPE_SUBMIT_INFO, NULL, 0, NULL, NULL, 1, &cb, 0, NULL};
    r2 = vkQueueSubmit(q, 1, &si, fence);
    check(r == 0 && r2 == 0, "vkQueueSubmit of an empty command buffer", r ? r : r2);
    r = vkWaitForFences(dev, 1, &fence, 1, 5000000000ull);
    check(r == 0, "vkWaitForFences (5 s)", r);

    /* Заполнение и копирование буфера. */
    const uint64_t size = 1 << 20;
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(pd, &mp);
    VkBuffer buf[2];
    VkDeviceMemory mem[2];
    uint32_t *ptr[2] = {NULL, NULL};
    int ok = 1;
    for (int i = 0; i < 2 && ok; i++) {
        VkBufferCreateInfo bci = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, size,
                                  VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT, 0, 0, NULL};
        if (vkCreateBuffer(dev, &bci, NULL, &buf[i])) ok = 0;
        VkMemoryRequirements mr;
        vkGetBufferMemoryRequirements(dev, buf[i], &mr);
        uint32_t want = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
        int type = -1;
        for (uint32_t t = 0; t < mp.memoryTypeCount && type < 0; t++)
            if ((mr.memoryTypeBits & (1u << t)) && (mp.memoryTypes[t].propertyFlags & want) == want) type = (int)t;
        VkMemoryAllocateInfo mai = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, NULL, mr.size, (uint32_t)type};
        if (type < 0 || vkAllocateMemory(dev, &mai, NULL, &mem[i]) || vkBindBufferMemory(dev, buf[i], mem[i], 0) ||
            vkMapMemory(dev, mem[i], 0, VK_WHOLE_SIZE, 0, (void **)&ptr[i]))
            ok = 0;
    }
    check(ok, "two 1 MiB host-visible coherent buffers mapped", 0);
    if (ok) {
        memset(ptr[0], 0x11, size);
        memset(ptr[1], 0x22, size);
        vkResetFences(dev, 1, &fence);
        vkResetCommandBuffer(cb, 0);
        vkBeginCommandBuffer(cb, &bi);
        vkCmdFillBuffer(cb, buf[0], 0, VK_WHOLE_SIZE, 0x12345678u);
        /* TRANSFER -> TRANSFER, запись -> чтение (глобальный барьер памяти). */
        struct { uint32_t sType; const void *pNext; uint32_t src, dst; } barrier = {46 /* MEMORY_BARRIER */, NULL, 0x1000, 0x800};
        vkCmdPipelineBarrier(cb, 0x1000, 0x1000, 0, 1, &barrier, 0, NULL, 0, NULL);
        VkBufferCopy region = {0, 0, size};
        vkCmdCopyBuffer(cb, buf[0], buf[1], 1, &region);
        r = vkEndCommandBuffer(cb);
        r2 = vkQueueSubmit(q, 1, &si, fence);
        VkResult r3 = vkWaitForFences(dev, 1, &fence, 1, 10000000000ull);
        check(r == 0 && r2 == 0 && r3 == 0, "fill + copy submitted and the fence signalled", r ? r : r2 ? r2 : r3);
        uint64_t bad = 0;
        for (uint64_t i = 0; i < size / 4; i++) bad += ptr[0][i] != 0x12345678u || ptr[1][i] != 0x12345678u;
        if (bad) printf("vkprobe: note words[0] 0x%08x 0x%08x\n", ptr[0][0], ptr[1][0]);
        check(bad == 0, "vkCmdFillBuffer + vkCmdCopyBuffer 1 MiB read back exactly", (long)bad);
    }
    vkDeviceWaitIdle(dev);
    vkDestroyDevice(dev, NULL);
    vkDestroyInstance(inst, NULL);
    printf("vkprobe: done\n");
    return finish();
}
