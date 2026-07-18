import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import bash from "highlight.js/lib/languages/bash";
import rust from "highlight.js/lib/languages/rust";
import ini from "highlight.js/lib/languages/ini";
import readme from "../../../../README.md?raw";

const prose =
  "text-sm leading-relaxed text-neutral-600 dark:text-neutral-400";
const heading =
  "font-semibold tracking-tight text-neutral-900 dark:text-neutral-100";
const codeInline =
  "rounded bg-neutral-100 px-1 py-0.5 font-mono text-[0.85em] text-neutral-800 dark:bg-neutral-800 dark:text-neutral-200";
const codeBlock =
  "hljs overflow-x-auto rounded-lg border border-neutral-200 bg-neutral-50 p-3 font-mono text-[0.8rem] text-neutral-800 dark:border-neutral-800 dark:bg-neutral-900 dark:text-neutral-200";

export function Readme() {
  return (
    <article className={`mt-10 space-y-4 ${prose}`}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[
          [
            rehypeHighlight,
            {
              languages: {
                rust,
                bash,
                shell: bash,
                ini,
                toml: ini,
              },
            },
          ],
        ]}
        components={{
          h1: ({ children }) => (
            <h2 className={`text-xl ${heading}`}>{children}</h2>
          ),
          h2: ({ children }) => (
            <h3 className={`pt-2 text-lg ${heading}`}>{children}</h3>
          ),
          h3: ({ children }) => (
            <h4 className={`pt-1 text-base ${heading}`}>{children}</h4>
          ),
          p: ({ children }) => <p>{children}</p>,
          ul: ({ children }) => (
            <ul className="list-disc space-y-1.5 pl-5">{children}</ul>
          ),
          ol: ({ children }) => (
            <ol className="list-decimal space-y-1.5 pl-5">{children}</ol>
          ),
          li: ({ children }) => <li>{children}</li>,
          a: ({ href, children }) => (
            <a
              href={href}
              target="_blank"
              rel="noopener noreferrer"
              className="text-primary underline-offset-2 hover:underline"
            >
              {children}
            </a>
          ),
          code: ({ className, children }) => {
            const isBlock = Boolean(className);
            if (isBlock) {
              return <code className={className}>{children}</code>;
            }
            return <code className={codeInline}>{children}</code>;
          },
          pre: ({ children }) => <pre className={codeBlock}>{children}</pre>,
          table: ({ children }) => (
            <div className="overflow-x-auto">
              <table className="w-full border-collapse text-left">{children}</table>
            </div>
          ),
          thead: ({ children }) => <thead>{children}</thead>,
          tbody: ({ children }) => <tbody>{children}</tbody>,
          tr: ({ children }) => <tr>{children}</tr>,
          th: ({ children }) => (
            <th className="border-b border-neutral-300 px-3 py-2 font-medium text-neutral-800 dark:border-neutral-700 dark:text-neutral-200">
              {children}
            </th>
          ),
          td: ({ children }) => (
            <td className="border-b border-neutral-200 px-3 py-2 dark:border-neutral-800">
              {children}
            </td>
          ),
        }}
      >
        {readme}
      </ReactMarkdown>
    </article>
  );
}
