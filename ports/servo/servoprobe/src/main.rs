//! `servoprobe` — Servo 0.8 на FreeOS. Первый шаг — движок собирается под
//! `<арх>-unknown-freeos` целиком (все крейты Servo, WebRender, SpiderMonkey,
//! шрифты, сеть). Дальше здесь будет страница без окна в PNG (N6).

extern crate servo;

fn main() {
    println!("servoprobe: servo linked");
}
