# dibibest

**Convert database schemas bolak-balik**: DBML (dbdiagram.io) ⇄ SQL DDL
(PostgreSQL, MySQL, SQLite) ⇄ Prisma ⇄ ERD-JSON, dengan **laporan fidelity** di
setiap konversi: apa yang selamat, apa yang bocor.

dbdiagram memberimu design-first; Liam ERD memberimu reverse-first. **dibibest
memberimu keduanya dalam satu IR**, dan mengukur sendiri apakah bolak-baliknya
lossless.

![dibibest workspace: DBML editor on the left, live ERD on the right, round-trip fidelity chip in the header](docs/screenshots/workspace-text-first.png)

> **Status: beta.** Format inti sudah jalan dan diuji round-trip di CI, tapi masih
> ada keterbatasan yang diketahui (lihat [di bawah](#keterbatasan-beta)). Kalau
> konversimu salah atau laporan fidelity-nya bohong, itu bug:
> [buka issue](https://github.com/codecoradev/dibibest/issues/new) dengan skema
> kecil yang mereproduksinya.

```bash
# DBML → SQL (PostgreSQL)
dibibest convert --to sql schema.dbml

# SQL → DBML (format sumber dideteksi otomatis)
cat schema.sql | dibibest convert --to dbml

# dialek & format lain
dibibest convert --to sql:mysql schema.dbml
dibibest convert --to prisma schema.dbml
dibibest formats            # dbml, sql, sql:mysql, sql:sqlite, prisma, erdjson
```

Contoh nyata, DBML → SQL:

```dbml
Table users {
  id integer [pk]
  email varchar [not null, unique, note: 'login']
  plan varchar [default: 'free']
}

Table orders {
  id integer [pk]
  user_id integer [not null]
}

Ref: orders.user_id > users.id [delete: cascade]
```

```sql
CREATE TABLE users (
  id integer PRIMARY KEY,
  email varchar NOT NULL UNIQUE,
  plan varchar DEFAULT 'free'
);

CREATE TABLE orders (
  id integer PRIMARY KEY,
  user_id integer NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
```

Laporan fidelity (ke stderr, jadi output tetap bisa di-pipe):

```
-- fidelity --
  tables   2/2 ✓
  columns  5/5 ✓
  refs     1/1 ✓
  indexes  0/0 ✓
  round-trip in source format: lossless ✓
  note: notes/comments have no equivalent in target format
```

## Kenapa fidelity report?

Konversi schema itu jujur atau tidak. Tool lain diam-diam membuang partial index,
enum, cascade, atau note. dibibest **selalu mengukur**:

- **kept/total per kategori.** Total dihitung dari teks sumber, tidak dari hasil
  parse, sehingga sesuatu yang hilang saat parse tetap terlihat. Ref dicocokkan
  per isi (tabel/kolom FK → tabel/kolom tujuan, plus `ON DELETE`/`ON UPDATE`),
  bukan sekadar dihitung, jadi ref yang terbalik atau kehilangan cascade tidak
  dianggap selamat.
- **`dropped`:** apa yang tidak punya padanan di format tujuan, disebut namanya.
- **Idempotensi:** `parse(emit(parse(src))) == parse(src)` di format sumber.

"Bolak-balik" bukan slogan; itu test yang jalan di setiap konversi dan di CI,
termasuk corpus 12 skema nyata yang dikonversi ke keenam format.

## Coba sekarang

Belum ada binary rilis; untuk beta, build dari source (butuh
[Rust stable](https://rustup.rs)).

### CLI

```bash
cargo install --git https://github.com/codecoradev/dibibest dibibest-cli
dibibest --help
```

### MCP (untuk agen AI)

```bash
cargo install --git https://github.com/codecoradev/dibibest dibibest-mcp
```

```json
{ "mcpServers": { "dibibest": { "command": "dibibest-mcp" } } }
```

Tools:

- `convert_schema` (`source`, `from?`, `to`) → output + fidelity
- `list_formats`

Manifest: [server.json](server.json).

### Web (ERD + editor, jalan sepenuhnya di browser)

```bash
rustup target add wasm32-unknown-unknown
cd apps/web && bun install && bun run dev
```

Tiga layout (text-first, canvas-first, explorer) di atas satu ERD-JSON; konversi
jalan lewat core yang sama, dikompilasi ke WASM. Tidak ada upload.

| Laporan fidelity (klik chip "round-trip") | Explorer: cari & sembunyikan tabel di skema besar |
|---|---|
| ![Fidelity report panel: kept/total for tables, columns, refs, indexes and round-trip idempotency](docs/screenshots/fidelity-panel.png) | ![Explorer layout: table list with search and visibility toggles next to the ERD canvas](docs/screenshots/explorer.png) |

Simpan proyek (opsional, OFF secara default) memakai `crates/dibibest-api` dengan
login OIDC. Lihat [docs/auth.md](docs/auth.md).

## Format

| Format | Parse | Emit | Catatan |
|---|---|---|---|
| DBML (dbdiagram) | ✓ | ✓ | Table/column + settings, indexes, Ref (`>` `<` `-`, composite, inline, blocks, `delete`/`update`), Enum, Project. Lihat [docs/dbml-gaps.md](docs/dbml-gaps.md) |
| SQL: PostgreSQL (`sql`) | ✓ | ✓ | via sqlparser-rs; `CREATE TABLE`/`INDEX`/`TYPE … AS ENUM`, FK + `ON DELETE`/`ON UPDATE`. Autoincrement belum ditulis (lihat bawah) |
| SQL: MySQL (`sql:mysql`) | ✓ | ✓ | backtick quoting, `AUTO_INCREMENT`; enum tidak ditulis (dilaporkan `dropped`). Lihat [docs/sql-dialects.md](docs/sql-dialects.md) |
| SQL: SQLite (`sql:sqlite`) | ✓ | ✓ | `AUTOINCREMENT`; enum tidak ditulis (dilaporkan `dropped`) |
| Prisma | ✓ | ✓ | subset, ditulis tangan: models, fields, `@id`/`@unique`/`@default`, `@relation` (+ `onDelete`/`onUpdate`), enums. Lihat [docs/prisma-integration.md](docs/prisma-integration.md) |
| ERD-JSON | ✓ | ✓ | kontrak kanvas: [docs/erdjson-schema.md](docs/erdjson-schema.md) |
| Live-DB introspect | – | – | roadmap |

## Keterbatasan beta

Yang **ditolak dengan error** (tidak pernah dibuang diam-diam) di DBML:
`TableGroup`, alias tabel (`Table users as U`), blok `Note { … }`, note
multi-baris (`'''…'''`), relasi many-to-many (`<>`), `checks`, `TablePartial`,
dan `Records`.

Yang diketahui masih salah dan sedang diperbaiki:

- **Expression/partial index** (`` (`lower(email)`) ``, `` (`… WHERE …`) ``) ke SQL
  belum menghasilkan SQL yang valid:
  [#49](https://github.com/codecoradev/dibibest/issues/49).
- **Autoincrement ke PostgreSQL** belum ditulis (`SERIAL`/`IDENTITY`); MySQL dan
  SQLite sudah. Lihat [docs/sql-dialects.md](docs/sql-dialects.md).

Yang berubah bentuk tapi tetap round-trip (kanonikalisasi):

- Komentar `//` di DBML dibuang.
- Ref inline dipindah ke baris `Ref:` sendiri.
- Urutan setting dinormalkan.

Rincian lengkap ada di [docs/dbml-gaps.md](docs/dbml-gaps.md). Progres menuju
rilis beta pertama dilacak di
[#44](https://github.com/codecoradev/dibibest/issues/44).

## Development

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # round-trip conformance + corpus lintas format
cargo run -p dibibest-cli -- --help
```

Roadmap: [docs/ROADMAP.md](docs/ROADMAP.md) · Arsitektur:
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) · Kontribusi:
[CONTRIBUTING.md](CONTRIBUTING.md)

## License

MIT. See [LICENSE](LICENSE).
