#version 450
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 color;
layout(binding = 0) uniform sampler2D prev;
layout(binding = 1) uniform sampler2D pattern;
layout(binding = 2) uniform Step { float step_; };
void main() {
  float n = mod(floor(texture(prev, uv).r * 255.0 + 0.5) + step_, 256.0);
  vec4 p = texture(pattern, uv);
  color = vec4(n / 255.0, p.g, p.b, 1.0);
}
