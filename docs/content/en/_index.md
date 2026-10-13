+++
title = "Mago"
description = "PHP#'s checker and toolchain: checks PHP and PHP# code, compiles PHP#, and lints and formats PHP."
nav_order = 10
nav_section = ""
+++
<section class="home-hero">

<div class="home-hero__main">

<div class="home-hero__plate"><span>Mago</span><span class="home-hero__plate-divider">/</span><span>PHP toolchain</span><span class="home-hero__plate-divider">/</span><span>mago-sharp</span></div>

<h1 class="home-hero__title">The checker and toolchain for <em>PHP#</em>.</h1>

<p class="home-hero__lede">mago-sharp is PHP#'s checker and toolchain, written in Rust. It checks PHP and PHP# code, compiles PHP# for the PHP# engine, and lints and formats PHP.</p>

<div class="home-hero__cta">
<a class="button button--solid" href="/guide/getting-started/"><span>Get started</span><span class="button__arrow">→</span></a>
</div>

</div>

<div class="home-hero__art">
<img class="home-hero__logo" src="/assets/logo.webp" alt="Mago, a fennec fox wearing a wizard's hat" width="416" height="500" loading="eager" decoding="async">
</div>

</section>

<section class="home-section">

<header class="home-section__head"><span class="home-section__num">§ 01</span><h2 class="home-section__title">Three tools, one binary</h2></header>

<div class="feature-grid">

<article class="feature">
<span class="feature__num">01 / Analyze</span>
<h3 class="feature__name">Static analysis</h3>
<p class="feature__body">Find bugs, dead code, and impossible types before they ship. Compatible with Psalm and PHPStan annotations; understands generics, conditional types, and flow narrowing.</p>
</article>

<article class="feature">
<span class="feature__num">02 / Lint</span>
<h3 class="feature__name">Opinionated linting</h3>
<p class="feature__body">A curated catalogue of rules for correctness, consistency, and clarity. Fix-on-save where safe. Quiet where it should be.</p>
</article>

<article class="feature">
<span class="feature__num">03 / Format</span>
<h3 class="feature__name">Formatter</h3>
<p class="feature__body">A deterministic formatter that produces stable, conventional output. No configuration roulette, no debate. Drop in and move on.</p>
</article>

</div>

</section>

<section class="home-section">

<header class="home-section__head"><span class="home-section__num">§ 02</span><h2 class="home-section__title">Install</h2></header>

<div class="install">
<div class="install__head"><span><strong>[ INSTALL ]</strong></span><span>shell · macOS · Linux · WSL</span></div>
<pre class="install__body"><code>curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/heyJordanParker/mago-sharp/master/scripts/install.sh | bash</code></pre>
<div class="install__alt">Or via <a href="/guide/installation/#composer">Composer</a>.</div>
</div>

</section>

<section class="home-section">

<header class="home-section__head"><span class="home-section__num">§ 03</span><h2 class="home-section__title">Three steps to first run</h2></header>

<ol class="home-steps">
<li><strong>Install.</strong> One command. No PHP runtime required. Single static binary.</li>
<li><strong>Initialize.</strong> Run <code>mago init</code> in your project root. Mago detects your layout and writes a <code>mago.toml</code>.</li>
<li><strong>Run.</strong> Use <code>mago analyze</code>, <code>mago lint</code>, or <code>mago fmt</code>. Wire it into pre-commit, CI, or your editor.</li>
</ol>

</section>
