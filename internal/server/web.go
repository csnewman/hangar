package server

import (
	"io/fs"
	"net/http"
	"path"
	"strings"
)

const notBuilt = `<!doctype html>
<title>Hangar</title>
<p>The web UI is not built into this binary. Run <code>npm run build</code> in
<code>web/</code> and rebuild, or use the development stack in
<code>compose.yaml</code>, which serves the UI from Vite.</p>
`

// webHandler serves the single-page app. A path that names a file serves the
// file; any other path serves index.html, so the app's own routes survive a
// reload.
func (s *Server) webHandler() http.Handler {
	if s.web == nil {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "text/html; charset=utf-8")
			_, _ = w.Write([]byte(notBuilt))
		})
	}
	files := http.FileServerFS(s.web)
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		name := strings.TrimPrefix(path.Clean(r.URL.Path), "/")
		if name != "" {
			if st, err := fs.Stat(s.web, name); err == nil && !st.IsDir() {
				// Vite puts a content hash in every asset's name, so an asset
				// never changes under the same URL.
				if strings.HasPrefix(name, "assets/") {
					w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
				}
				files.ServeHTTP(w, r)
				return
			}
		}
		w.Header().Set("Cache-Control", "no-cache")
		http.ServeFileFS(w, r, s.web, "index.html")
	})
}
