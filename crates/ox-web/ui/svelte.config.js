import adapter from '@sveltejs/adapter-static';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	kit: {
		adapter: adapter({
			pages: '../../../target/js-pkg',
			assets: '../../../target/js-pkg',
			fallback: 'index.html'
		}),
		paths: {
			relative: true
		}
	}
};

export default config;
