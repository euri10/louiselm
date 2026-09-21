// Serve only the candidate artifact, with the isolation required by Neovim WASM.
import { createServer } from 'node:http';
import { readFile, realpath } from 'node:fs/promises';
import { extname, resolve, sep } from 'node:path';

const root = await realpath(process.env.LOUISELM_SITE_ARTIFACT || '_build/html');
await readFile(resolve(root, 'demo/index.html')); // Fail immediately on a missing build.
const types = {
	'.html': 'text/html; charset=utf-8', '.js': 'application/javascript',
	'.css': 'text/css', '.json': 'application/json', '.wasm': 'application/wasm',
};

createServer(async (request, response) => {
	response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
	response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
	response.setHeader('Cache-Control', 'no-store');
	if (!['GET', 'HEAD'].includes(request.method)) {
		response.writeHead(405).end();
		return;
	}
	try {
		const pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
		const path = await realpath(resolve(root, `.${pathname}${pathname.endsWith('/') ? 'index.html' : ''}`));
		if (!path.startsWith(root + sep)) {
			response.writeHead(403).end();
			return;
		}
		const content = await readFile(path);
		response.writeHead(200, { 'Content-Type': types[extname(path)] || 'application/octet-stream' });
		response.end(request.method === 'HEAD' ? undefined : content);
	} catch (error) {
		if (error.code === 'ENOENT' || error.code === 'ENOTDIR') response.writeHead(404).end();
		else if (error instanceof URIError) response.writeHead(400).end();
		else {
			console.error(error);
			response.writeHead(500).end();
		}
	}
}).listen(8765, '127.0.0.1');
