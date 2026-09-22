# Guest programs

Native programs installed into every base that can run them. They are compiled
by the image builder (`internal/image/scripts/mkosi.sh`), not in the image, so
no base carries a toolchain.

- `hangar-glcheck.c` renders continuously on the guest's GPU and checks every
  frame against what it must be. Its purpose is to make "did the GPU state
  survive a suspend" something a test can observe: a lost texture, shader,
  sampler, vertex buffer or framebuffer shows up as a specific wrong pixel, and
  a lost context as a reset.
- `hangar-vkcheck.c` does the same through Vulkan, with `vkcheck.vert` and
  `vkcheck.frag` compiled to SPIR-V at build time. It also leans on what a
  Vulkan program holds for its whole life: command buffers recorded once and
  resubmitted every frame, descriptor sets written once, and a uniform buffer
  that stays mapped.
