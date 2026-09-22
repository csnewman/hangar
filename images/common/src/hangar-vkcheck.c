// hangar-vkcheck renders continuously through Vulkan and checks every frame
// against what it must be, so a guest can tell whether its Vulkan state
// survived a suspend.
//
// The rendering is the same as hangar-glcheck's: each frame draws into one of
// two images while sampling the other, so the red channel counts frames and
// green and blue come from a pattern uploaded once. What differs is what it
// leans on, chosen to be what a Vulkan program holds for its whole life:
//
//   - two command buffers recorded once, at startup, and submitted again
//     every frame, so a frame is only right if their recorded contents
//     survive;
//   - two descriptor sets written once;
//   - a uniform buffer that stays mapped, written through the mapping;
//   - images left in shader-read layout between frames.
//
// It exits 2 on a wrong pixel and 3 on a Vulkan error, having said which.

#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "vkcheck_vert.h"
#include "vkcheck_frag.h"

#define W 64
#define H 64

static void check(VkResult r, const char *what, unsigned long frame) {
    if (r != VK_SUCCESS) {
        printf("vkcheck: %s failed (%d) at frame %lu\n", what, r, frame);
        fflush(stdout);
        exit(3);
    }
}
#define CK(x) check((x), #x, 0)

static VkPhysicalDevice phys;
static VkDevice dev;

static uint32_t memtype(uint32_t bits, VkMemoryPropertyFlags want) {
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(phys, &mp);
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++)
        if ((bits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & want) == want) return i;
    printf("vkcheck: no memory type\n");
    exit(1);
}

static VkDeviceMemory alloc(VkMemoryRequirements req, VkMemoryPropertyFlags want) {
    VkMemoryAllocateInfo ai = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, NULL, req.size,
                               memtype(req.memoryTypeBits, want)};
    VkDeviceMemory m;
    CK(vkAllocateMemory(dev, &ai, NULL, &m));
    return m;
}

static void buffer(VkDeviceSize size, VkBufferUsageFlags usage, VkBuffer *b, VkDeviceMemory *m, void **map) {
    VkBufferCreateInfo bi = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, size, usage,
                             VK_SHARING_MODE_EXCLUSIVE, 0, NULL};
    CK(vkCreateBuffer(dev, &bi, NULL, b));
    VkMemoryRequirements req;
    vkGetBufferMemoryRequirements(dev, *b, &req);
    *m = alloc(req, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    CK(vkBindBufferMemory(dev, *b, *m, 0));
    CK(vkMapMemory(dev, *m, 0, VK_WHOLE_SIZE, 0, map));
}

static void image(VkImageUsageFlags usage, VkImage *img, VkImageView *view) {
    VkImageCreateInfo ii = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, NULL, 0, VK_IMAGE_TYPE_2D,
                            VK_FORMAT_R8G8B8A8_UNORM, {W, H, 1}, 1, 1, VK_SAMPLE_COUNT_1_BIT,
                            VK_IMAGE_TILING_OPTIMAL, usage, VK_SHARING_MODE_EXCLUSIVE, 0, NULL,
                            VK_IMAGE_LAYOUT_UNDEFINED};
    CK(vkCreateImage(dev, &ii, NULL, img));
    VkMemoryRequirements req;
    vkGetImageMemoryRequirements(dev, *img, &req);
    VkDeviceMemory m = alloc(req, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    CK(vkBindImageMemory(dev, *img, m, 0));
    VkImageViewCreateInfo vi = {VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, *img,
                                VK_IMAGE_VIEW_TYPE_2D, VK_FORMAT_R8G8B8A8_UNORM, {0},
                                {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}};
    CK(vkCreateImageView(dev, &vi, NULL, view));
}

static void barrier(VkCommandBuffer cb, VkImage img, VkImageLayout from, VkImageLayout to,
                    VkAccessFlags src, VkAccessFlags dst, VkPipelineStageFlags ss, VkPipelineStageFlags ds) {
    VkImageMemoryBarrier b = {VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, src, dst, from, to,
                              VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, img,
                              {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}};
    vkCmdPipelineBarrier(cb, ss, ds, 0, 0, NULL, 0, NULL, 1, &b);
}

int main(int argc, char **argv) {
    int fps = argc > 1 ? atoi(argv[1]) : 30;
    if (fps <= 0) fps = 30;

    VkApplicationInfo app = {VK_STRUCTURE_TYPE_APPLICATION_INFO, NULL, "hangar-vkcheck", 1, NULL, 0, VK_API_VERSION_1_1};
    VkInstanceCreateInfo ici = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, NULL, 0, &app, 0, NULL, 0, NULL};
    VkInstance inst;
    CK(vkCreateInstance(&ici, NULL, &inst));
    uint32_t n = 1;
    VkResult er = vkEnumeratePhysicalDevices(inst, &n, &phys);
    if ((er != VK_SUCCESS && er != VK_INCOMPLETE) || n == 0) {
        printf("vkcheck: no physical device\n");
        return 1;
    }
    VkPhysicalDeviceProperties props;
    vkGetPhysicalDeviceProperties(phys, &props);

    float prio = 1.0f;
    VkDeviceQueueCreateInfo qci = {VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, NULL, 0, 0, 1, &prio};
    VkDeviceCreateInfo dci = {VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, NULL, 0, 1, &qci, 0, NULL, 0, NULL, NULL};
    CK(vkCreateDevice(phys, &dci, NULL, &dev));
    VkQueue q;
    vkGetDeviceQueue(dev, 0, 0, &q);
    printf("vkcheck: device %s, %d fps\n", props.deviceName, fps);
    fflush(stdout);

    // Images: two ping-pong targets and the pattern.
    VkImage img[3];
    VkImageView view[3];
    for (int i = 0; i < 2; i++)
        image(VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_SAMPLED_BIT |
              VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT, &img[i], &view[i]);
    image(VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT, &img[2], &view[2]);

    // Buffers: vertices, the uniform, upload staging and readback, all
    // mapped for the life of the program.
    static const float quad[] = {-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1};
    VkBuffer vbuf, ubuf, sbuf, rbuf;
    VkDeviceMemory vmem, umem, smem, rmem;
    void *vmap, *umap, *smap, *rmap;
    buffer(sizeof quad, VK_BUFFER_USAGE_VERTEX_BUFFER_BIT, &vbuf, &vmem, &vmap);
    memcpy(vmap, quad, sizeof quad);
    buffer(16, VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT, &ubuf, &umem, &umap);
    buffer(W * H * 4, VK_BUFFER_USAGE_TRANSFER_SRC_BIT, &sbuf, &smem, &smap);
    buffer(W * H * 4, VK_BUFFER_USAGE_TRANSFER_DST_BIT, &rbuf, &rmem, &rmap);
    unsigned char *pat = smap;
    for (int y = 0; y < H; y++)
        for (int x = 0; x < W; x++) {
            unsigned char *p = &pat[(y * W + x) * 4];
            p[0] = 0; p[1] = (unsigned char)(x * 4); p[2] = (unsigned char)(y * 4); p[3] = 255;
        }
    // The step lives in mapped memory, read by the shader every frame.
    ((float *)umap)[0] = 1.0f;

    VkSamplerCreateInfo sci = {VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO, NULL, 0, VK_FILTER_NEAREST, VK_FILTER_NEAREST,
                               VK_SAMPLER_MIPMAP_MODE_NEAREST, VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
                               VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
                               0, VK_FALSE, 1, VK_FALSE, VK_COMPARE_OP_ALWAYS, 0, 0,
                               VK_BORDER_COLOR_FLOAT_TRANSPARENT_BLACK, VK_FALSE};
    VkSampler sampler;
    CK(vkCreateSampler(dev, &sci, NULL, &sampler));

    VkDescriptorSetLayoutBinding binds[3] = {
        {0, VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 1, VK_SHADER_STAGE_FRAGMENT_BIT, NULL},
        {1, VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 1, VK_SHADER_STAGE_FRAGMENT_BIT, NULL},
        {2, VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, 1, VK_SHADER_STAGE_FRAGMENT_BIT, NULL}};
    VkDescriptorSetLayoutCreateInfo dli = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, NULL, 0, 3, binds};
    VkDescriptorSetLayout dsl;
    CK(vkCreateDescriptorSetLayout(dev, &dli, NULL, &dsl));
    VkDescriptorPoolSize ps[2] = {{VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 4}, {VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, 2}};
    VkDescriptorPoolCreateInfo dpi = {VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, NULL, 0, 2, 2, ps};
    VkDescriptorPool pool;
    CK(vkCreateDescriptorPool(dev, &dpi, NULL, &pool));
    VkDescriptorSetLayout layouts[2] = {dsl, dsl};
    VkDescriptorSetAllocateInfo dai = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, NULL, pool, 2, layouts};
    VkDescriptorSet sets[2];
    CK(vkAllocateDescriptorSets(dev, &dai, sets));
    for (int i = 0; i < 2; i++) {
        // Set i renders into image i, so it samples the other.
        VkDescriptorImageInfo prev = {sampler, view[!i], VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
        VkDescriptorImageInfo pattern = {sampler, view[2], VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
        VkDescriptorBufferInfo ub = {ubuf, 0, 16};
        VkWriteDescriptorSet w[3] = {
            {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, NULL, sets[i], 0, 0, 1, VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, &prev, NULL, NULL},
            {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, NULL, sets[i], 1, 0, 1, VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, &pattern, NULL, NULL},
            {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, NULL, sets[i], 2, 0, 1, VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, NULL, &ub, NULL}};
        vkUpdateDescriptorSets(dev, 3, w, 0, NULL);
    }

    VkAttachmentDescription att = {0, VK_FORMAT_R8G8B8A8_UNORM, VK_SAMPLE_COUNT_1_BIT,
                                   VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_STORE,
                                   VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_DONT_CARE,
                                   VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL};
    VkAttachmentReference ref = {0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    VkSubpassDescription sub = {0, VK_PIPELINE_BIND_POINT_GRAPHICS, 0, NULL, 1, &ref, NULL, NULL, 0, NULL};
    VkRenderPassCreateInfo rpi = {VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO, NULL, 0, 1, &att, 1, &sub, 0, NULL};
    VkRenderPass rp;
    CK(vkCreateRenderPass(dev, &rpi, NULL, &rp));
    VkFramebuffer fb[2];
    for (int i = 0; i < 2; i++) {
        VkFramebufferCreateInfo fi = {VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, NULL, 0, rp, 1, &view[i], W, H, 1};
        CK(vkCreateFramebuffer(dev, &fi, NULL, &fb[i]));
    }

    VkShaderModuleCreateInfo vsi = {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, NULL, 0, sizeof vkcheck_vert, vkcheck_vert};
    VkShaderModuleCreateInfo fsi = {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, NULL, 0, sizeof vkcheck_frag, vkcheck_frag};
    VkShaderModule vs, fs;
    CK(vkCreateShaderModule(dev, &vsi, NULL, &vs));
    CK(vkCreateShaderModule(dev, &fsi, NULL, &fs));
    VkPipelineLayoutCreateInfo pli = {VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, NULL, 0, 1, &dsl, 0, NULL};
    VkPipelineLayout pl;
    CK(vkCreatePipelineLayout(dev, &pli, NULL, &pl));
    VkPipelineShaderStageCreateInfo stages[2] = {
        {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, VK_SHADER_STAGE_VERTEX_BIT, vs, "main", NULL},
        {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, VK_SHADER_STAGE_FRAGMENT_BIT, fs, "main", NULL}};
    VkVertexInputBindingDescription vb = {0, 8, VK_VERTEX_INPUT_RATE_VERTEX};
    VkVertexInputAttributeDescription va = {0, 0, VK_FORMAT_R32G32_SFLOAT, 0};
    VkPipelineVertexInputStateCreateInfo vis = {VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO, NULL, 0, 1, &vb, 1, &va};
    VkPipelineInputAssemblyStateCreateInfo ias = {VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, NULL, 0, VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST, VK_FALSE};
    VkViewport vp = {0, 0, W, H, 0, 1};
    VkRect2D sc = {{0, 0}, {W, H}};
    VkPipelineViewportStateCreateInfo vps = {VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, NULL, 0, 1, &vp, 1, &sc};
    VkPipelineRasterizationStateCreateInfo rs = {VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO, NULL, 0, VK_FALSE, VK_FALSE,
                                                 VK_POLYGON_MODE_FILL, VK_CULL_MODE_NONE, VK_FRONT_FACE_COUNTER_CLOCKWISE, VK_FALSE, 0, 0, 0, 1};
    VkPipelineMultisampleStateCreateInfo ms = {VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, NULL, 0, VK_SAMPLE_COUNT_1_BIT, VK_FALSE, 0, NULL, VK_FALSE, VK_FALSE};
    VkPipelineColorBlendAttachmentState cba = {VK_FALSE, 0, 0, 0, 0, 0, 0, 0xf};
    VkPipelineColorBlendStateCreateInfo cbs = {VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, NULL, 0, VK_FALSE, 0, 1, &cba, {0}};
    VkGraphicsPipelineCreateInfo gpi = {VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO, NULL, 0, 2, stages, &vis, &ias, NULL, &vps, &rs, &ms,
                                        NULL, &cbs, NULL, pl, rp, 0, VK_NULL_HANDLE, -1};
    VkPipeline pipe;
    CK(vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gpi, NULL, &pipe));

    VkCommandPoolCreateInfo cpi = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, NULL, 0, 0};
    VkCommandPool cp;
    CK(vkCreateCommandPool(dev, &cpi, NULL, &cp));
    VkCommandBuffer cb[3];
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, NULL, cp, VK_COMMAND_BUFFER_LEVEL_PRIMARY, 3};
    CK(vkAllocateCommandBuffers(dev, &cai, cb));
    VkFence fence;
    VkFenceCreateInfo fci = {VK_STRUCTURE_TYPE_FENCE_CREATE_INFO, NULL, 0};
    CK(vkCreateFence(dev, &fci, NULL, &fence));
    VkCommandBufferBeginInfo bi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, NULL, 0, NULL};
    VkBufferImageCopy region = {0, 0, 0, {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, {0, 0, 0}, {W, H, 1}};

    // Setup, once: the pattern uploaded, both targets cleared to zero and
    // left in the layout a frame expects to find the image it samples in.
    CK(vkBeginCommandBuffer(cb[2], &bi));
    barrier(cb[2], img[2], VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 0, VK_ACCESS_TRANSFER_WRITE_BIT,
            VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
    vkCmdCopyBufferToImage(cb[2], sbuf, img[2], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region);
    barrier(cb[2], img[2], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_SHADER_READ_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT);
    VkClearColorValue zero = {{0, 0, 0, 0}};
    VkImageSubresourceRange rng = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    for (int i = 0; i < 2; i++) {
        barrier(cb[2], img[i], VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 0, VK_ACCESS_TRANSFER_WRITE_BIT,
                VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
        vkCmdClearColorImage(cb[2], img[i], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &zero, 1, &rng);
        barrier(cb[2], img[i], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_SHADER_READ_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT);
    }
    CK(vkEndCommandBuffer(cb[2]));
    VkSubmitInfo si = {VK_STRUCTURE_TYPE_SUBMIT_INFO, NULL, 0, NULL, NULL, 1, &cb[2], 0, NULL};
    CK(vkQueueSubmit(q, 1, &si, fence));
    CK(vkWaitForFences(dev, 1, &fence, VK_TRUE, UINT64_MAX));
    CK(vkResetFences(dev, 1, &fence));

    // The frame command buffers, recorded once. cb[i] renders into image i
    // sampling the other, reads image i back, and leaves it where the next
    // frame will sample it.
    for (int i = 0; i < 2; i++) {
        CK(vkBeginCommandBuffer(cb[i], &bi));
        VkRenderPassBeginInfo rbi = {VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, NULL, rp, fb[i], {{0, 0}, {W, H}}, 0, NULL};
        vkCmdBeginRenderPass(cb[i], &rbi, VK_SUBPASS_CONTENTS_INLINE);
        vkCmdBindPipeline(cb[i], VK_PIPELINE_BIND_POINT_GRAPHICS, pipe);
        vkCmdBindDescriptorSets(cb[i], VK_PIPELINE_BIND_POINT_GRAPHICS, pl, 0, 1, &sets[i], 0, NULL);
        VkDeviceSize off = 0;
        vkCmdBindVertexBuffers(cb[i], 0, 1, &vbuf, &off);
        vkCmdDraw(cb[i], 6, 1, 0, 0);
        vkCmdEndRenderPass(cb[i]);
        vkCmdCopyImageToBuffer(cb[i], img[i], VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, &region);
        barrier(cb[i], img[i], VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                VK_ACCESS_TRANSFER_READ_BIT, VK_ACCESS_SHADER_READ_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT);
        CK(vkEndCommandBuffer(cb[i]));
    }

    for (unsigned long frame = 1;; frame++) {
        int dst = frame & 1;
        si.pCommandBuffers = &cb[dst];
        check(vkQueueSubmit(q, 1, &si, fence), "vkQueueSubmit", frame);
        check(vkWaitForFences(dev, 1, &fence, VK_TRUE, 5000000000ull), "vkWaitForFences", frame);
        check(vkResetFences(dev, 1, &fence), "vkResetFences", frame);

        const unsigned char *got = rmap;
        for (int y = 0; y < H; y++)
            for (int x = 0; x < W; x++) {
                const unsigned char *g = &got[(y * W + x) * 4];
                unsigned char want[4] = {(unsigned char)(frame & 0xff), (unsigned char)(x * 4),
                                         (unsigned char)(y * 4), 255};
                if (memcmp(g, want, 4) != 0) {
                    printf("vkcheck: frame %lu wrong at (%d,%d): got %u,%u,%u,%u want %u,%u,%u,%u\n",
                           frame, x, y, g[0], g[1], g[2], g[3], want[0], want[1], want[2], want[3]);
                    fflush(stdout);
                    return 2;
                }
            }
        if (frame % (unsigned long)fps == 0) {
            printf("vkcheck: frame %lu ok\n", frame);
            fflush(stdout);
        }
        usleep(1000000 / fps);
    }
}
