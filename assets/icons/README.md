# FreeOS Icons 1.0

69 линейных иконок, сетка 24×24, `stroke="currentColor"`, `stroke-width="1.75"`, скруглённые концы и углы.

- `freeos-icons.js` — подключает спрайт и даёт `FreeOSIcons.svg(name,{size,stroke,color})`
- `sprite.svg` — все иконки как `<symbol id="fi-имя">`
- `svg/*.svg` — отдельные файлы

```html
<script src="freeos-icons/freeos-icons.js"></script>
<svg width="24" height="24" viewBox="0 0 24 24" style="fill:none;stroke:currentColor;stroke-width:1.75;stroke-linecap:round;stroke-linejoin:round"><use href="#fi-home"></use></svg>
```
