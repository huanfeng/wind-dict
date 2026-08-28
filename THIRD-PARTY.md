# 第三方数据与许可

本程序自身的代码为 MIT OR Apache-2.0。随程序分发的**数据文件**另有其上游与协议，
逐份列在下面。三者协议不同，这正是它们各占一个 `.db` 文件、不合并的原因——一旦合进
同一个文件，署名义务就再也拆不开了。

| 数据文件 | 上游 | 协议 |
|---|---|---|
| `ecdict.db` | ECDICT | MIT |
| `cedict.db` | CC-CEDICT | CC BY-SA 4.0 |
| `unihan.db` | Unicode Han Database (Unihan) | Unicode License V3 |

---

## ECDICT

英汉词库。上游：<https://github.com/skywind3000/ECDICT>，MIT 许可。

`ecdict.db` 由上游的 `ecdict.csv` 转换而来，见 `examples/build_ecdict.rs` 与
`docs/adr/0010`。

## CC-CEDICT

汉英词库。上游：<https://www.mdbg.net/chinese/dictionary?page=cc-cedict>，
采用 **Creative Commons Attribution-ShareAlike 4.0 International**
(<https://creativecommons.org/licenses/by-sa/4.0/>)。

`cedict.db` 是 CC-CEDICT 的**衍生数据**，因此该文件本身亦以 CC BY-SA 4.0 分发，
并保留上述署名。本程序的代码不受此条款影响（数据与代码是可分离的）。

转换工具见 `examples/build_cedict.rs`。

## Unihan

字形库（部首、笔画、繁简对应）。上游：
<https://www.unicode.org/Public/UCD/latest/ucd/Unihan.zip>，规范见 UAX #38
(<https://www.unicode.org/reports/tr38/>)。

`unihan.db` 由上游数据文件转换而来，见 `examples/build_unihan.rs` 与
`docs/adr/0013`。Unicode License V3 唯一的强制条件是随数据或文档保留下列声明，
本文件即为履行该条件而存在：

```
Copyright © 1991-2025 Unicode, Inc.

NOTICE TO USER: Carefully read the following legal agreement. BY
DOWNLOADING, INSTALLING, COPYING OR OTHERWISE USING DATA FILES, AND/OR
SOFTWARE, YOU UNEQUIVOCALLY ACCEPT, AND AGREE TO BE BOUND BY, ALL OF THE
TERMS AND CONDITIONS OF THIS AGREEMENT. IF YOU DO NOT AGREE, DO NOT
DOWNLOAD, INSTALL, COPY, DISTRIBUTE OR USE THE DATA FILES OR SOFTWARE.

Permission is hereby granted, free of charge, to any person obtaining a
copy of data files and any associated documentation (the "Data Files") or
software and any associated documentation (the "Software") to deal in the
Data Files or Software without restriction, including without limitation
the rights to use, copy, modify, merge, publish, distribute, and/or sell
copies of the Data Files or Software, and to permit persons to whom the
Data Files or Software are furnished to do so, provided that either (a)
this copyright and permission notice appear with all copies of the Data
Files or Software, or (b) this copyright and permission notice appear in
associated Documentation.

THE DATA FILES AND SOFTWARE ARE PROVIDED "AS IS", WITHOUT WARRANTY OF ANY
KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT OF
THIRD PARTY RIGHTS. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR HOLDERS
INCLUDED IN THIS NOTICE BE LIABLE FOR ANY CLAIM, OR ANY SPECIAL INDIRECT
OR CONSEQUENTIAL DAMAGES, OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF
USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR
OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR
PERFORMANCE OF THE DATA FILES OR SOFTWARE.

Except as contained in this notice, the name of a copyright holder shall
not be used in advertising or otherwise to promote the sale, use or other
dealings in these Data Files or Software without prior written
authorization of the copyright holder.
```

许可全文：<https://www.unicode.org/license.txt>
