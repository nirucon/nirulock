# Credits

## The Erik Satie portrait (`layout = "satie"`)

The `satie` layout shows a C64 style conversion of a photograph of Erik Satie
(1866 to 1925) by Henri Manuel (1874 to 1947), taken about 1920.

- Source: Wikimedia Commons, `File:Ericsatie.jpg`,
  <https://commons.wikimedia.org/wiki/File:Ericsatie.jpg>
  (original: <https://upload.wikimedia.org/wikipedia/commons/5/58/Ericsatie.jpg>,
  2651x3711 pixels, SHA-256
  `75fa8d543850a1261ae5a5f8030cdd61140c94397360986f43878cea100a119b`).
- Licence on the file page: `{{PD-Art|PD-old-auto-expired|deathyear=1947}}`,
  public domain (the photographer died in 1947, more than 70 years ago).
  Checked on 2026-11-26.
- Commons description: "Portrait d'Erik Satie par Henri Manuel, vers 1920 (?).
  27 x 21 cm. Dédicace : A Valentine Gross." Source given there: Étude
  Ader-Tajan, vente Jacques Guérin n°7, 25 May 1992, lot 104.

Only the converted picture is in this repository: `assets/satie.bin`, a
160x200 bitmap with six brightness levels after ordered dithering (16,000
bytes), made offline and reproducibly with

```sh
python3 -I tools/satie-picture.py Ericsatie.jpg assets/satie.bin
```

which gives SHA-256
`70304d40d44bef8f761f33ca943b27cbfc4e922689147d18eb6738339f1d16b8`. The colours
come from the active NIRUX theme at run time.

## The pixel font

NIRUSID's 8x8 font (`src/pixfont.rs`), drawn for NIRUSID, MIT like this
program. It is not the Commodore character set.

## The NIRUX BBS screen (`layout = "bbs"`)

The ANSI screen `assets/nirux-bbs.ans` is original work made for NIRULOCK,
MIT like this program. There is no source image: `tools/bbs-art.py` paints
Yggdrasil from shapes (balls of leaves lit from the upper right, the trunk,
the branches, the roots, the ground, the full moon and the stars), fits
it cell by cell to the CP437 block and shade characters with two colours
each, draws the NIRUX logo from a small pixel font of its own, adds the
frames and labels, and writes the file with a SAUCE record. It is
reproducible:

```sh
python3 -I tools/bbs-art.py assets/nirux-bbs.ans
```

which gives 8,710 bytes with SHA-256
`5115b094f692c87bb031a4d7e3c0769d282e7f7523af14e4520ca90dd4b739e5`.

The modem handshake is synthesized by NIRULOCK at run time (`src/modem.rs`);
no recording is used. The dialled number 555-64789 is made up (555 is the
exchange films use, 64789 spells NIRUX on a phone keypad).

## The NIRUX intrusion console (`layout = "hacker"`)

Everything in it is original work made for NIRULOCK, MIT like this program,
and drawn by code at run time (`src/hacker.rs`): the boot text, the panes,
the wire-frame globe and the mesh, the dot-matrix clock and the analyser.
It is a homage to the look of hacker films and cyberpunk fiction in
general; no logo, name, screen or text of any film or book is used or
recreated. The nodes are Norse names (the nine realms, Yggdrasil and
Bifrost), and all memory, traffic, routes and round trip times are made up
from a random seed; nothing is read from the network or the system. The
characters are NIRUSID's 8x8 font as in the `bbs` layout.
