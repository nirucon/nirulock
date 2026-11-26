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
