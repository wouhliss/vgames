export function Placeholder({ title }: { title: string }) {
  return (
    <section aria-labelledby="page-title">
      <h1 id="page-title">{title}</h1>
      <p className="muted">This page is being built.</p>
    </section>
  );
}
