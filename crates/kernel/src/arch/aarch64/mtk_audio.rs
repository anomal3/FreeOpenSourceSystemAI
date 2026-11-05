//! Голос в разговоре: AFE MT6765 + кодек MT6357 + PCM модема (06.11).
//!
//! # Как идёт звук
//!
//! Целиком железом, без копирования процессором: речь модема (MD1 PCM) входит
//! в блок AFE «MODEM_PCM_2» (`PCM2_INTF_CON`), межсоединения AFE (`AFE_CONN*`)
//! отдают её в ЦАП (ADDA DL), ЦАП по MTKAIF уходит в кодек MT6357 (он в
//! микросхеме питания) → разговорный динамик; микрофон → АЦП MT6357 → MTKAIF
//! → AFE (ADDA UL) → межсоединения → PCM модема. Модему же по каналу CCCI 5
//! (`ccci_aud`) говорится «речь вкл.» — `SPH_ON`, ответ `0xAF20` по каналу 4.
//!
//! Последовательности — из ядра MediaTek для MT6765
//! (`sound/soc/mediatek/common_int/mtk-soc-pcm-voice-md1.c`,
//! `mtk-soc-afe-control.c`, `mt6765/mt6765-sound.c`,
//! `codec/mt6357/mtk-soc-codec-6357.c`: `TurnOnDacPower`,
//! `Voice_Amp_Change`, `TurnOnADcPowerACC`); идентификаторы сообщений речи —
//! восстановлены по журналу Android (`SPH_ON = 0x2F20` подтверждён ответом,
//! `SPH_OFF = 0x2F21` — догадка).
//!
//! Только 8 кГц (узкая полоса); 16 кГц — `wide`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::mtk_regs::{pmic_read, pmic_write, read32, write32};

const AFE: u64 = 0x1122_0000;
const INFRA: u64 = 0x1000_1000;
const TOPCK: u64 = 0x1000_0000;
const GPIO_MODE_136: u64 = 0x1000_5410; // выводы 136..143, по 4 бита

// AFE (смещения от 0x11220000).
const AUDIO_TOP_CON0: u64 = 0x000;
const AFE_DAC_CON0: u64 = 0x010;
const AFE_CONN3: u64 = 0x02C;
const AFE_CONN4: u64 = 0x030;
const AFE_I2S_CON1: u64 = 0x034;
const DL_SRC2_CON0: u64 = 0x108;
const DL_SRC2_CON1: u64 = 0x10C;
const UL_SRC_CON0: u64 = 0x114;
const ADDA_TOP_CON0: u64 = 0x120;
const UL_DL_CON0: u64 = 0x124;
const UL_SRC_MON0: u64 = 0x148;
const PREDIS_CON0: u64 = 0x260;
const PREDIS_CON1: u64 = 0x264;
const IIR_COEF: [(u64, u32); 5] =
    [(0x290, 0x0000_0000), (0x294, 0x0000_3FB8), (0x298, 0x3FB8_0000), (0x29C, 0x3FB8_0000), (0x2A0, 0x0000_C048)];
const DAC_MON: u64 = 0x2EC;
const AFE_CONN17: u64 = 0x460;
const AFE_CONN18: u64 = 0x464;
const AFE_CONN28: u64 = 0x4BC;
const AFE_CONN29: u64 = 0x4C0;
const PCM2_INTF_CON: u64 = 0x53C;
const SDM_DCCOMP_CON: u64 = 0xC50;
const MTKAIF_CFG0: u64 = 0xE00;
const MTKAIF_RX_CFG0: u64 = 0xE20;
const AUD_PAD_TOP_CFG: u64 = 0xE40;

/// Идентификаторы речи модему (канал CCCI 5).
pub const SPH_ON: u32 = 0x2F20;
pub const SPH_OFF: u32 = 0x2F21;
const DL_GAIN: u32 = 0x2F00;
const UL_GAIN: u32 = 0x2F01;

fn afe(off: u64) -> u32 {
    read32(AFE + off).unwrap_or(0)
}

fn afe_set(off: u64, value: u32) {
    write32(AFE + off, value);
}

fn afe_mod(off: u64, mask: u32, value: u32) {
    let old = afe(off);
    afe_set(off, (old & !mask) | (value & mask));
}

/// PMIC: запись под маской (как `Ana_Set_Reg`).
fn ana(addr: u16, value: u16, mask: u16) {
    let old = pmic_read(addr).unwrap_or(0);
    let _ = pmic_write(addr, (old & !mask) | (value & mask));
}

fn delay(us: u64) {
    super::timer::delay_us(us);
}

/// Такты аудио: infracfg (`ifr_audio`, `ifr_audio26m`), мультиплексоры
/// topckgen, вентили `AUDIO_TOP_CON0`.
fn clocks_on() {
    write32(INFRA + 0x8C, 1 << 25); // ifr_audio
    write32(INFRA + 0xA8, 1 << 4); // ifr_audio26m (MTKAIF)
    // audio_sel = 26 МГц (CLK_CFG_3 [25:24], вентиль 31) — снять вентиль.
    write32(TOPCK + 0x78, (3 << 24) | (1 << 31));
    // aud_intbus_sel = syspll1_d4 (CLK_CFG_4 [1:0] = 1, вентиль 7).
    write32(TOPCK + 0x88, 3 | (1 << 7));
    write32(TOPCK + 0x84, 1);
    write32(TOPCK + 0x04, (1 << 15) | (1 << 16));
    // AFE, DAC, DAC_PREDIS; ADC — позже.
    afe_mod(AUDIO_TOP_CON0, (1 << 2) | (1 << 25) | (1 << 26), 0);
}

/// Выводы MTKAIF (136, 138, 139 — к кодеку; 140, 142, 143 — от него) в
/// функцию 1.
fn pins_on() {
    let mut value = read32(GPIO_MODE_136).unwrap_or(0);
    for pin in [136u32, 138, 139, 140, 142, 143] {
        let shift = (pin - 136) * 4;
        value = (value & !(0xF << shift)) | (1 << shift);
    }
    write32(GPIO_MODE_136, value);
}

/// Кодек: общее включение тракта ЦАП (`TurnOnDacPower`).
fn codec_dac_on() {
    ana(0x07AC, 1 << 13, 1 << 13); // DCXO_CW14: audck buffer
    let _ = pmic_write(0x00BA, 0xFFFF); // GPIO_MODE2_CLR
    let _ = pmic_write(0x00B8, 0x0249); // GPIO_MODE2_SET
    let _ = pmic_write(0x00B6, 0x0249); // GPIO_MODE2
    ana(0x002C, 0x0FF0, 0x0FF0); // SMT_CON1
    ana(0x2214, 0x0280, 0x0FFF); // AUDDEC_ANA_CON6
    ana(0x2216, 0x00A8, 0xFFFF); // AUDDEC_ANA_CON7
    ana(0x2216, 0x0088, 0xFFFF);
    ana(0x221E, 0, 1 << 4); // AUDDEC_ANA_CON11: NVREG
    ana(0x220C, 0x0400, 0x0400); // pull-down
    ana(0x2194, 1, 1); // AUDENC_ANA_CON6: CLSQ
    ana(0x208C, 0, 0x66); // AUD_TOP_CKPDN_CON0
    delay(250);
    ana(0x2112, 0, 0x00C4); // PMIC_AUDIO_TOP_CON0
    delay(250);
    let _ = pmic_write(0x211A, 0x0006); // AFUNC_AUD_CON2
    let _ = pmic_write(0x2116, 0xCBA1); // AFUNC_AUD_CON0
    let _ = pmic_write(0x211A, 0x0003);
    let _ = pmic_write(0x211A, 0x000B);
    ana(0x2108, 1, 1); // AFE_UL_DL_CON0
    let _ = pmic_write(0x210A, 1); // AFE_DL_SRC2_CON0_L
    let _ = pmic_write(0x2110, 0); // PMIC_AFE_TOP_CON0
}

/// Кодек: разговорный динамик (`Voice_Amp_Change(true)`).
fn codec_earpiece_on() {
    let _ = pmic_write(0x2208, 0x3000); // AUDDEC_ANA_CON0
    let _ = pmic_write(0x220E, 0x0010); // CON3
    let _ = pmic_write(0x2210, 0x0010); // CON4
    ana(0x220C, 0x0200, 0x0200);
    let _ = pmic_write(0x228E, 0x1F); // ZCD_CON3: минимум
    let _ = pmic_write(0x20B6, 1); // NCP
    let _ = pmic_write(0x20B8, 0x2C);
    let _ = pmic_write(0x20B4, 1);
    let _ = pmic_write(0x20BC, 2);
    let _ = pmic_write(0x20BA, 0);
    delay(250);
    ana(0x2220, 0x1055, 0x1055); // AUDDEC_ANA_CON12
    let _ = pmic_write(0x2222, 1); // CON13
    delay(100);
    let _ = pmic_write(0x2288, 0); // ZCD_CON0
    let _ = pmic_write(0x221C, 0x0055); // CON10
    let _ = pmic_write(0x221A, 0x0092); // CON9
    let _ = pmic_write(0x221C, 0x0055);
    for v in [0x0090, 0x0092, 0x0093] {
        let _ = pmic_write(0x220E, v);
    }
    let _ = pmic_write(0x228E, 8); // ZCD_CON3: 0 дБ
    ana(0x221E, 1, 1); // CON11 bit0
    let _ = pmic_write(0x2208, 0x3009); // ЦАП вкл.
    ana(0x2214, 0x0281, 0x0FFF);
    let _ = pmic_write(0x220E, 0x009B); // мультиплексор HS → ЦАП
    ana(0x220C, 0, 0x0400); // pull-down выкл.
}

/// Кодек: основной микрофон AMIC0 (`TurnOnADcPowerACC`).
fn codec_mic_on() {
    let _ = pmic_write(0x00C0, 0xFFFF); // GPIO_MODE3_CLR
    let _ = pmic_write(0x00BE, 0x0249);
    let _ = pmic_write(0x00BC, 0x0249);
    ana(0x221E, 1 << 5, 1 << 5); // ADC CLKGEN
    let _ = pmic_write(0x210E, 0); // AUDENC_ANA_CON3
    ana(0x2220, 0x0100, 0x2500);
    ana(0x2220, 0x2500, 0x2500); // LCLDO_ENC
    ana(0x2198, 0x0021, 0xFFFF); // MICBIAS0 1,9 В
    ana(0x2188, 3 << 8, 0x0700); // PGA
    ana(0x2188, 0x0041, 0xF0FF); // AIN0, PGA вкл.
    ana(0x2188, 0x5041, 0xF000); // АЦП вкл.
    ana(0x2112, 0x8000, 0xDFBF);
    let _ = pmic_write(0x2110, 0);
    ana(0x2108, 1, 1);
    let _ = pmic_write(0x2134, 0); // MTKAIF_CFG0
    ana(0x214C, 0x3100, 0xFF00); // AFE_AUD_PAD_TOP
    let _ = pmic_write(0x210C, 0); // UL_SRC_CON0_H
    let _ = pmic_write(0x210E, 1); // UL_SRC_CON0_L
}

/// Сторона AP: межсоединения, DL, UL, включение, PCM2 к модему.
fn afe_on(wide: bool) {
    // A. Межсоединения: АЦП → модем, модем → ЦАП (и ЦАП_2).
    afe_mod(AFE_CONN17, 1 << 3, 1 << 3);
    afe_mod(AFE_CONN18, 1 << 4, 1 << 4);
    afe_mod(AFE_CONN3, 1 << 14, 1 << 14);
    afe_mod(AFE_CONN4, 1 << 14, 1 << 14);
    afe_mod(AFE_CONN28, 1 << 14, 1 << 14);
    afe_mod(AFE_CONN29, 1 << 14, 1 << 14);
    // B. DL.
    afe_set(PREDIS_CON0, 0);
    afe_set(PREDIS_CON1, 0);
    afe_set(DL_SRC2_CON0, if wide { 0x3300_1822 } else { 0x0300_1822 });
    afe_set(DL_SRC2_CON1, 0xF74F_0000);
    afe_mod(SDM_DCCOMP_CON, 0x3F, 0x1D);
    afe_set(AFE_I2S_CON1, if wide { 0x40A } else { 0x0A });
    // C. UL.
    afe_set(AUD_PAD_TOP_CFG, 0x31);
    afe_set(MTKAIF_CFG0, 0);
    afe_mod(ADDA_TOP_CON0, 1, 0);
    for (off, v) in IIR_COEF {
        afe_set(off, v);
    }
    afe_set(UL_SRC_CON0, if wide { 0x0002_0400 } else { 0x0000_0400 });
    afe_mod(MTKAIF_RX_CFG0, 1, 0);
    // D. Включение.
    afe_mod(AFE_DAC_CON0, 1, 1);
    afe_mod(UL_DL_CON0, 1, 1);
    afe_mod(DL_SRC2_CON0, 1, 1);
    afe_mod(AFE_I2S_CON1, 1, 1);
    afe_mod(AUDIO_TOP_CON0, 1 << 24, 0); // такт АЦП
    afe_mod(UL_SRC_CON0, 1, 1);
    afe_set(PCM2_INTF_CON, if wide { 0x4E } else { 0x46 });
    afe_mod(PCM2_INTF_CON, 1, 1);
}

fn afe_off() {
    afe_mod(AFE_CONN17, 1 << 3, 0);
    afe_mod(AFE_CONN18, 1 << 4, 0);
    for off in [AFE_CONN3, AFE_CONN4, AFE_CONN28, AFE_CONN29] {
        afe_mod(off, 1 << 14, 0);
    }
    afe_mod(AFE_I2S_CON1, 1, 0);
    afe_mod(PCM2_INTF_CON, 1, 0);
    afe_mod(DL_SRC2_CON0, 1, 0);
    afe_mod(UL_SRC_CON0, 1, 0);
    afe_mod(UL_DL_CON0, 1, 0);
    delay(125);
    afe_mod(AUDIO_TOP_CON0, 1 << 24, 1 << 24);
    afe_mod(AFE_DAC_CON0, 1, 0);
}

fn codec_off() {
    // Микрофон.
    ana(0x2188, 0, 0xF0FF);
    ana(0x2198, 0, 0xFFFF);
    let _ = pmic_write(0x210E, 0);
    // Динамик: мультиплексор и ЦАП выкл., усилитель выкл.
    ana(0x220C, 0x0400, 0x0400);
    let _ = pmic_write(0x220E, 0x0010);
    let _ = pmic_write(0x2208, 0x0000);
    ana(0x2220, 0, 0x1055);
    let _ = pmic_write(0x20B4, 0); // NCP
    ana(0x2108, 0, 1);
    ana(0x208C, 0x66, 0x66);
    ana(0x221E, 1 << 4, 1 << 4);
}

/// Речь модему: команда без данных (`data0 = 0xFFFFFFFF`,
/// `data1 = id << 16 | param`) по каналу 5.
fn speech(id: u32, param: u32) -> Result<(), String> {
    super::mtk_ccci::send_pcm_msg((id << 16) | (param & 0xFFFF))
}

/// Включить голос: такты, выводы, AFE, кодек, `SPH_ON`, усиления.
pub fn voice_on(wide: bool) -> Vec<String> {
    let mut out = Vec::new();
    clocks_on();
    pins_on();
    codec_dac_on();
    codec_earpiece_on();
    codec_mic_on();
    afe_on(wide);
    out.push(format!("AFE on: TOP {:08x} DAC {:08x} PCM2 {:08x}", afe(AUDIO_TOP_CON0), afe(AFE_DAC_CON0), afe(PCM2_INTF_CON)));
    for (id, p) in [(SPH_ON, 0), (DL_GAIN, 0), (UL_GAIN, 0x34)] {
        if let Err(err) = speech(id, p) {
            out.push(format!("speech {id:#x}: {err}"));
        }
    }
    out.push(String::from("SPH_ON sent, ack 0xAF20 expected on ch 4 (oem md log)"));
    crate::kprintln!("  audio       : voice on ({})", if wide { "16 kHz" } else { "8 kHz" });
    out
}

/// Выключить голос.
pub fn voice_off() -> Vec<String> {
    let mut out = Vec::new();
    if let Err(err) = speech(SPH_OFF, 0) {
        out.push(format!("SPH_OFF: {err}"));
    }
    afe_off();
    codec_off();
    afe_mod(AUDIO_TOP_CON0, (1 << 2) | (1 << 25) | (1 << 26), (1 << 2) | (1 << 25) | (1 << 26));
    out.push(String::from("voice off"));
    crate::kprintln!("  audio       : voice off");
    out
}

/// Снимок: ключевые регистры AFE и кодека.
pub fn status() -> Vec<String> {
    let p = |a: u16| pmic_read(a).map_or_else(|_| String::from("?"), |v| format!("{v:04x}"));
    alloc::vec![
        format!("TOP {:08x} DAC {:08x} UL_DL {:08x} MON {:08x}", afe(AUDIO_TOP_CON0), afe(AFE_DAC_CON0), afe(UL_DL_CON0), afe(DAC_MON)),
        format!("DL2 {:08x} I2S1 {:08x} UL {:08x} PCM2 {:08x}", afe(DL_SRC2_CON0), afe(AFE_I2S_CON1), afe(UL_SRC_CON0), afe(PCM2_INTF_CON)),
        format!("UL mon {:08x} {:08x} pins {:08x}", afe(UL_SRC_MON0), afe(UL_SRC_MON0 + 4), read32(GPIO_MODE_136).unwrap_or(0)),
        format!("dec0 {} dec3 {} enc0 {} mb {} top {}", p(0x2208), p(0x220E), p(0x2188), p(0x2198), p(0x2112)),
    ]
}

/// Голос по ходу вызова: включить при «разговоре», выключить при отбое.
pub fn follow_call(state: u8, on: &mut bool) {
    let talking = state == user_abi::CALL_ACTIVE;
    if talking && !*on {
        *on = true;
        voice_on(false);
    } else if !talking && *on && state != user_abi::CALL_ALERTING && state != user_abi::CALL_DIALING {
        *on = false;
        voice_off();
    }
}

