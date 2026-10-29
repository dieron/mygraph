# Vendored browser libraries

Served by the server from the binary (no CDN), for the graph viewer ([ADMIN-UI-2]).

| File | Library | Version | Licence | Source |
|---|---|---|---|---|
| `cytoscape-3.30.4.min.js` | Cytoscape.js | 3.30.4 | MIT ([cytoscape-LICENSE](cytoscape-LICENSE)) | npm `cytoscape@3.30.4`, `dist/cytoscape.min.js`; tarball integrity `sha512-OxtlZwQl1WbwMmLiyPSEBuzeTIQnwZhJYYWFzZ2PhEHVFwpeaqNIkUzSiso00D98qk60l8Gwon2RP304d3BJ1A==` |

To update: `npm pack cytoscape@<version>`, check the integrity against `npm view`, copy
`package/dist/cytoscape.min.js` and `package/LICENSE` here, and change the file name in
`src/lib.rs` and `ui/graph.html`.
