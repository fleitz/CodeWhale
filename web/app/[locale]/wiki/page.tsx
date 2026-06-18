import Link from "next/link";
import { Seal } from "@/components/seal";
import { buildPageMetadata } from "@/lib/page-meta";
import { WIKI_PAGES, wikiHref, wikiStatusClass, wikiStatusLabel } from "@/lib/wiki";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const isZh = locale === "zh";
  return buildPageMetadata({
    path: "/wiki",
    locale,
    title: isZh ? "Wiki · CodeWhale" : "Wiki · CodeWhale",
    description: isZh
      ? "CodeWhale 由递归子 Agent 生成的源码地图：架构、工具、RLM、Whaleflow、Fleet 与运行时内部机制。"
      : "The recursive sub-agent generated source map for CodeWhale: architecture, tools, RLM, Whaleflow, Fleet, and runtime internals.",
  });
}

export default async function WikiPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const isZh = locale === "zh";

  return (
    <>
      <section className="mx-auto max-w-[1400px] px-6 pt-12 pb-8">
        <div className="mb-3 flex items-baseline gap-4">
          <Seal char={isZh ? "图" : "W"} />
          <div className="eyebrow">{isZh ? "Section 03 · 源码地图" : "Section 03 · Source Map"}</div>
        </div>
        <h1 className="font-display tracking-crisp">
          Wiki <span className="font-cjk ml-2 text-5xl text-indigo">{isZh ? "源码地图" : "Source Map"}</span>
        </h1>
        <p className="mt-5 max-w-3xl text-lg leading-[1.9] tracking-wide text-ink-soft">
          {isZh
            ? "这套 wiki 是用 CodeWhale 自己的递归子 Agent 系统从源码生成的。它适合做维护者地图：哪些系统已经上线，哪些只是协议或实验性运行时。"
            : "This wiki was generated from the source by CodeWhale's own recursive sub-agent system. Treat it as a maintainer map: what is live, what is protocol-level, and what is still experimental runtime work."}
        </p>
        <div className="mt-6 flex flex-wrap gap-3">
          <Link
            href={wikiHref(locale, WIKI_PAGES[0])}
            className="inline-flex bg-indigo px-4 py-2 font-mono text-xs uppercase tracking-wider text-paper transition-colors hover:bg-indigo-deep"
          >
            {isZh ? "阅读第一章" : "Read Chapter 01"}
          </Link>
          <Link
            href={isZh ? "/zh/docs" : "/en/docs"}
            className="inline-flex hairline-t hairline-b hairline-l hairline-r px-4 py-2 font-mono text-xs uppercase tracking-wider transition-colors hover:bg-paper-deep"
          >
            {isZh ? "返回文档" : "Back to Docs"}
          </Link>
        </div>
      </section>

      <section className="mx-auto grid max-w-[1400px] grid-cols-1 gap-px bg-paper-line px-6 pb-20 md:grid-cols-2 xl:grid-cols-3">
        {WIKI_PAGES.map((page) => (
          <Link
            key={page.file}
            href={wikiHref(locale, page)}
            className="group min-w-0 bg-paper p-6 transition-colors hover:bg-paper-deep"
          >
            <div className="mb-4 flex items-start justify-between gap-4 hairline-b pb-3">
              <div className="font-mono text-xs uppercase tracking-[0.18em] text-ink-mute">{page.id}</div>
              <div className={`font-mono text-[0.68rem] uppercase tracking-[0.14em] ${wikiStatusClass(page.status)}`}>
                {wikiStatusLabel(page.status, isZh)}
              </div>
            </div>
            <h2 className="text-2xl">{page.title}</h2>
            <p className="mt-3 text-sm leading-[1.8] tracking-wide text-ink-soft">
              {isZh ? page.cn : page.summary}
            </p>
            <div className="mt-5 font-mono text-[0.72rem] uppercase tracking-wider text-indigo">
              {page.file} <span className="transition-transform group-hover:translate-x-1">-&gt;</span>
            </div>
          </Link>
        ))}
      </section>

      <section className="bg-ink text-paper">
        <div className="mx-auto grid max-w-[1400px] gap-8 px-6 py-12 lg:grid-cols-12">
          <div className="lg:col-span-7">
            <div className="font-cjk mb-2 text-lg text-indigo">{isZh ? "发布边界" : "Release Boundary"}</div>
            <h2 className="text-3xl text-paper">
              {isZh ? "0.8.63 应该发布地图，不应该夸大运行时。" : "0.8.63 should ship the map, not overstate the runtime."}
            </h2>
            <p className="mt-3 max-w-2xl leading-[1.8] tracking-wide text-paper-deep/80">
              {isZh
                ? "子 Agent、RLM、skills、hooks、MCP、sandbox 与 snapshot 是实际可用的基础。Whaleflow、Fleet 和 Workroom 页面保留实验性标签，直到 core/TUI/runtime API 真的调用这些路径。"
                : "Sub-agents, RLM, skills, hooks, MCP, sandboxing, and snapshots are usable foundations today. Whaleflow, Fleet, and Workroom pages keep experimental labels until core, TUI, and Runtime API paths actually execute them."}
            </p>
          </div>
          <div className="lg:col-span-5">
            <div className="hairline-t hairline-b border-paper-deep/30 py-4">
              <div className="font-mono text-xs uppercase tracking-widest text-paper-deep/60">
                {isZh ? "建议位置" : "Recommended Location"}
              </div>
              <div className="mt-2 font-display text-2xl text-paper">/wiki</div>
              <p className="mt-2 text-sm leading-[1.8] text-paper-deep/75">
                {isZh
                  ? "网站展示索引、状态和章节正文；Markdown 仍作为源码随仓库一起审查和打 tag。"
                  : "The site shows the index, status, and chapter content; Markdown stays in the repository as reviewed, tagged source."}
              </p>
            </div>
          </div>
        </div>
      </section>
    </>
  );
}
