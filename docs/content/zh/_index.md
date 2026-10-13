+++
title = "Mago"
description = "PHP# 的检查器与工具链:检查 PHP 与 PHP# 代码,编译 PHP#,并对 PHP 代码进行 lint 和格式化。"
nav_order = 10
nav_section = ""
+++
<section class="home-hero">

<div class="home-hero__main">

<div class="home-hero__plate"><span>Mago</span><span class="home-hero__plate-divider">/</span><span>PHP 工具链</span><span class="home-hero__plate-divider">/</span><span>mago-sharp</span></div>

<h1 class="home-hero__title"><em>PHP#</em> 的检查器与工具链。</h1>

<p class="home-hero__lede">mago-sharp 是用 Rust 编写的 PHP# 检查器与工具链。它检查 PHP 与 PHP# 代码,为 PHP# 引擎编译 PHP#,并对 PHP 代码进行 lint 和格式化。</p>

<div class="home-hero__cta">
<a class="button button--solid" href="/guide/getting-started/"><span>快速开始</span><span class="button__arrow">→</span></a>
</div>

</div>

<div class="home-hero__art">
<img class="home-hero__logo" src="/assets/logo.webp" alt="Mago, 头戴巫师帽的耳廓狐" width="416" height="500" loading="eager" decoding="async">
</div>

</section>

<section class="home-section">

<header class="home-section__head"><span class="home-section__num">§ 01</span><h2 class="home-section__title">三款工具,一个二进制</h2></header>

<div class="feature-grid">

<article class="feature">
<span class="feature__num">01 / Analyze</span>
<h3 class="feature__name">静态分析</h3>
<p class="feature__body">在代码上线前发现 bug、死代码和不可能的类型。兼容 Psalm 和 PHPStan 注解;理解泛型、条件类型和流向收窄。</p>
</article>

<article class="feature">
<span class="feature__num">02 / Lint</span>
<h3 class="feature__name">有主张的 lint 检查</h3>
<p class="feature__body">面向正确性、一致性与清晰度的精选规则集合。安全时保存即修复。无需时保持安静。</p>
</article>

<article class="feature">
<span class="feature__num">03 / Format</span>
<h3 class="feature__name">格式化器</h3>
<p class="feature__body">一款确定性的格式化器,产出稳定且符合惯例的输出。无需纠结配置,没有无谓争论。开箱即用,无需多虑。</p>
</article>

</div>

</section>

<section class="home-section">

<header class="home-section__head"><span class="home-section__num">§ 02</span><h2 class="home-section__title">安装</h2></header>

<div class="install">
<div class="install__head"><span><strong>[ INSTALL ]</strong></span><span>shell · macOS · Linux · WSL</span></div>
<pre class="install__body"><code>curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/heyJordanParker/mago-sharp/master/scripts/install.sh | bash</code></pre>
<div class="install__alt">或通过 <a href="/guide/installation/#composer">Composer</a>。</div>
</div>

</section>

<section class="home-section">

<header class="home-section__head"><span class="home-section__num">§ 03</span><h2 class="home-section__title">三步上手</h2></header>

<ol class="home-steps">
<li><strong>安装。</strong>一条命令。无需 PHP 运行时。单一静态二进制。</li>
<li><strong>初始化。</strong>在项目根目录运行 <code>mago init</code>。Mago 会探测你的项目布局并写入 <code>mago.toml</code>。</li>
<li><strong>运行。</strong>使用 <code>mago analyze</code>、<code>mago lint</code> 或 <code>mago fmt</code>。把它接入 pre-commit、CI 或编辑器。</li>
</ol>

</section>
