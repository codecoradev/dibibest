<script lang="ts">
	// The dibibest mark: d and b mirrored about a centre axis, i.e. "both ways".
	// Same drawing as static/favicon.svg.
	let { size = 24, label }: { size?: number; label?: string } = $props();
</script>

<svg
	class="logo"
	width={size}
	height={size}
	viewBox="0 0 64 64"
	role={label ? 'img' : undefined}
	aria-label={label}
	aria-hidden={label ? undefined : 'true'}
>
	<defs>
		<linearGradient id="dibibest-logo-g" x1="0" y1="0" x2="1" y2="1">
			<stop offset="0" stop-color="#4f8cff" />
			<stop offset="1" stop-color="#8a5cff" />
		</linearGradient>
	</defs>
	<rect width="64" height="64" rx="15" fill="url(#dibibest-logo-g)" />
	<g fill="none" stroke="#0b0d10" stroke-width="6" stroke-linecap="round">
		<circle cx="19.5" cy="41" r="8.5" />
		<circle cx="44.5" cy="41" r="8.5" />
		<path d="M28 15v34.5M36 15v34.5" />
	</g>
</svg>

<style>
	.logo {
		display: block;
		flex: 0 0 auto;
	}
</style>
