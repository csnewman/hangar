// hangar-glcheck renders continuously and checks every frame against what it
// must be, so a guest can tell whether its GPU state survived a suspend.
//
// Each frame draws into one of two textures, sampling the other, so the image
// is the product of every frame before it: the red channel counts frames, and
// green and blue come from a pattern texture uploaded once at startup. The
// whole frame is read back and compared. A frame is therefore only right if
// the texture contents, the program, its uniforms and samplers, the vertex
// buffer and the framebuffers are all what they were -- which is everything a
// renderer has to carry across a suspend for a program to keep going.
//
// It exits 2 on a wrong pixel, 3 if the context reports it was reset, and 4 on
// any other GL error, having said which on stdout.

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl3.h>
#include <GLES2/gl2ext.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define W 64
#define H 64

static const char *vs_src =
    "#version 300 es\n"
    "in vec2 pos;\n"
    "out vec2 uv;\n"
    "void main() { uv = pos * 0.5 + 0.5; gl_Position = vec4(pos, 0.0, 1.0); }\n";

static const char *fs_src =
    "#version 300 es\n"
    "precision highp float;\n"
    "in vec2 uv;\n"
    "uniform sampler2D prev;\n"
    "uniform sampler2D pattern;\n"
    "uniform float step_;\n"
    "out vec4 color;\n"
    "void main() {\n"
    "  float n = mod(floor(texture(prev, uv).r * 255.0 + 0.5) + step_, 256.0);\n"
    "  vec4 p = texture(pattern, uv);\n"
    "  color = vec4(n / 255.0, p.g, p.b, 1.0);\n"
    "}\n";

static void die(int code, const char *fmt, unsigned long frame) {
    printf(fmt, frame);
    printf("\n");
    fflush(stdout);
    exit(code);
}

static GLuint shader(GLenum type, const char *src) {
    GLuint s = glCreateShader(type);
    glShaderSource(s, 1, &src, NULL);
    glCompileShader(s);
    GLint ok = 0;
    glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[1024];
        glGetShaderInfoLog(s, sizeof log, NULL, log);
        printf("glcheck: shader: %s\n", log);
        exit(1);
    }
    return s;
}

static GLuint texture(const unsigned char *data) {
    GLuint t;
    glGenTextures(1, &t);
    glBindTexture(GL_TEXTURE_2D, t);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, W, H, 0, GL_RGBA, GL_UNSIGNED_BYTE, data);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    return t;
}

int main(int argc, char **argv) {
    int fps = argc > 1 ? atoi(argv[1]) : 30;
    if (fps <= 0) fps = 30;

    PFNEGLGETPLATFORMDISPLAYEXTPROC get_display =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    if (!get_display) die(1, "glcheck: no eglGetPlatformDisplayEXT%.0lu", 0);
    EGLDisplay dpy = get_display(EGL_PLATFORM_SURFACELESS_MESA, EGL_DEFAULT_DISPLAY, NULL);
    if (dpy == EGL_NO_DISPLAY || !eglInitialize(dpy, NULL, NULL))
        die(1, "glcheck: no EGL display%.0lu", 0);
    eglBindAPI(EGL_OPENGL_ES_API);

    EGLint cattr[] = {EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT, EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_NONE};
    EGLConfig cfg;
    EGLint n = 0;
    if (!eglChooseConfig(dpy, cattr, &cfg, 1, &n) || n == 0)
        die(1, "glcheck: no ES3 config%.0lu", 0);

    // A robust context reports a reset instead of failing in ways that look
    // like wrong rendering, which is the difference this program exists to
    // tell apart.
    int robust = strstr(eglQueryString(dpy, EGL_EXTENSIONS), "EGL_EXT_create_context_robustness") != NULL;
    EGLint rattr[] = {EGL_CONTEXT_CLIENT_VERSION, 3,
                      EGL_CONTEXT_OPENGL_ROBUST_ACCESS_EXT, EGL_TRUE,
                      EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT, EGL_LOSE_CONTEXT_ON_RESET_EXT,
                      EGL_NONE};
    EGLint pattr[] = {EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE};
    EGLContext ctx = robust ? eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, rattr) : EGL_NO_CONTEXT;
    if (ctx == EGL_NO_CONTEXT) {
        robust = 0;
        ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, pattr);
    }
    if (ctx == EGL_NO_CONTEXT || !eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, ctx))
        die(1, "glcheck: no context%.0lu", 0);
    PFNGLGETGRAPHICSRESETSTATUSEXTPROC reset_status =
        robust ? (PFNGLGETGRAPHICSRESETSTATUSEXTPROC)eglGetProcAddress("glGetGraphicsResetStatusEXT") : NULL;

    printf("glcheck: renderer %s, robust %s, %d fps\n", glGetString(GL_RENDERER),
           reset_status ? "yes" : "no", fps);
    fflush(stdout);

    GLuint prog = glCreateProgram();
    glAttachShader(prog, shader(GL_VERTEX_SHADER, vs_src));
    glAttachShader(prog, shader(GL_FRAGMENT_SHADER, fs_src));
    glBindAttribLocation(prog, 0, "pos");
    glLinkProgram(prog);
    glUseProgram(prog);
    glUniform1i(glGetUniformLocation(prog, "prev"), 0);
    glUniform1i(glGetUniformLocation(prog, "pattern"), 1);
    glUniform1f(glGetUniformLocation(prog, "step_"), 1.0f);

    static const float quad[] = {-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1};
    GLuint vao, vbo;
    glGenVertexArrays(1, &vao);
    glBindVertexArray(vao);
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof quad, quad, GL_STATIC_DRAW);
    glEnableVertexAttribArray(0);
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, 0);

    static unsigned char pat[W * H * 4], zero[W * H * 4], got[W * H * 4];
    for (int y = 0; y < H; y++)
        for (int x = 0; x < W; x++) {
            unsigned char *p = &pat[(y * W + x) * 4];
            p[0] = 0; p[1] = (unsigned char)(x * 4); p[2] = (unsigned char)(y * 4); p[3] = 255;
        }
    GLuint pattern = texture(pat);
    GLuint tex[2] = {texture(zero), texture(zero)};
    GLuint fbo[2];
    glGenFramebuffers(2, fbo);
    for (int i = 0; i < 2; i++) {
        glBindFramebuffer(GL_FRAMEBUFFER, fbo[i]);
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex[i], 0);
        if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE)
            die(1, "glcheck: framebuffer incomplete%.0lu", 0);
    }
    glViewport(0, 0, W, H);
    glActiveTexture(GL_TEXTURE1);
    glBindTexture(GL_TEXTURE_2D, pattern);

    for (unsigned long frame = 1;; frame++) {
        int dst = frame & 1, src = !dst;
        glBindFramebuffer(GL_FRAMEBUFFER, fbo[dst]);
        glActiveTexture(GL_TEXTURE0);
        glBindTexture(GL_TEXTURE_2D, tex[src]);
        glDrawArrays(GL_TRIANGLES, 0, 6);
        glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, got);

        if (reset_status) {
            GLenum r = reset_status();
            if (r != GL_NO_ERROR) {
                printf("glcheck: context reset (0x%x) at frame %lu\n", r, frame);
                fflush(stdout);
                return 3;
            }
        }
        GLenum e = glGetError();
        if (e != GL_NO_ERROR) {
            printf("glcheck: GL error 0x%x at frame %lu\n", e, frame);
            fflush(stdout);
            return 4;
        }
        for (int y = 0; y < H; y++)
            for (int x = 0; x < W; x++) {
                const unsigned char *g = &got[(y * W + x) * 4];
                unsigned char want[4] = {(unsigned char)(frame & 0xff), (unsigned char)(x * 4),
                                         (unsigned char)(y * 4), 255};
                if (memcmp(g, want, 4) != 0) {
                    printf("glcheck: frame %lu wrong at (%d,%d): got %u,%u,%u,%u want %u,%u,%u,%u\n",
                           frame, x, y, g[0], g[1], g[2], g[3], want[0], want[1], want[2], want[3]);
                    fflush(stdout);
                    return 2;
                }
            }
        if (frame % (unsigned long)fps == 0) {
            printf("glcheck: frame %lu ok\n", frame);
            fflush(stdout);
        }
        usleep(1000000 / fps);
    }
}
