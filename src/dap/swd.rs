//! SWD 傳輸層：DAP_Transfer/TransferBlock 與低階 swd_transfer/idle（自 mod.rs 抽出，R2）。
use super::Dap;
use super::types::*;

impl<'d> Dap<'d> {
    // ---------------- Transfer / TransferBlock ----------------

    /// DAP_Transfer：處理多筆任意傳輸（含 posted AP read）。
    pub(crate) async fn cmd_transfer(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let _dap_index = req[1];
        let count = req[2];
        let mut qi = 3; // 請求游標
        let mut ri = 3; // 回應資料游標（resp[1]=count, resp[2]=ack）
        let mut processed = 0u8;
        let mut ack = Ack::Ok;
        let mut post_read = false; // 是否有尚未取回的 posted AP read

        'outer: for _ in 0..count {
            let request = req[qi];
            qi += 1;

            if request & REQ_RNW != 0 {
                // 讀取
                if request & REQ_APND_P != 0 {
                    // AP read：posted
                    if post_read {
                        // 取回前一筆 posted 結果並再 post 這次
                        let (a, val) = self.transfer_retry(request, 0).await;
                        ack = a;
                        if a != Ack::Ok {
                            break 'outer;
                        }
                        put_u32_le(&mut resp[ri..], val);
                        ri += 4;
                    } else {
                        let (a, _) = self.transfer_retry(request, 0).await;
                        ack = a;
                        if a != Ack::Ok {
                            break 'outer;
                        }
                        post_read = true;
                    }
                } else {
                    // DP read：立即
                    if post_read {
                        // 先用 RDBUFF 取回 posted AP read
                        let (a, val) = self.transfer_retry(DP_RDBUFF_RD, 0).await;
                        ack = a;
                        if a != Ack::Ok {
                            break 'outer;
                        }
                        put_u32_le(&mut resp[ri..], val);
                        ri += 4;
                        post_read = false;
                    }
                    let (a, val) = self.transfer_retry(request, 0).await;
                    ack = a;
                    if a != Ack::Ok {
                        break 'outer;
                    }
                    put_u32_le(&mut resp[ri..], val);
                    ri += 4;
                }
            } else {
                // 寫入
                if post_read {
                    let (a, val) = self.transfer_retry(DP_RDBUFF_RD, 0).await;
                    ack = a;
                    if a != Ack::Ok {
                        break 'outer;
                    }
                    put_u32_le(&mut resp[ri..], val);
                    ri += 4;
                    post_read = false;
                }
                let data = u32_le(&req[qi..qi + 4]);
                qi += 4;
                let (a, _) = self.transfer_retry(request, data).await;
                ack = a;
                if a != Ack::Ok {
                    break 'outer;
                }
            }
            processed += 1;
        }

        // 收尾：取回仍 pending 的 posted read
        if post_read && ack == Ack::Ok {
            let (a, val) = self.transfer_retry(DP_RDBUFF_RD, 0).await;
            ack = a;
            if a == Ack::Ok {
                put_u32_le(&mut resp[ri..], val);
                ri += 4;
            }
        }

        resp[1] = processed;
        resp[2] = ack.to_byte();
        ri
    }

    /// DAP_TransferBlock：對同一暫存器連續多筆讀或寫。
    pub(crate) async fn cmd_transfer_block(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let _dap_index = req[1];
        let count = u16::from_le_bytes([req[2], req[3]]) as u32;
        let request = req[4];
        let mut qi = 5;
        let mut ri = 4; // resp[1..3]=count, resp[3]=ack
        let mut processed: u16 = 0;
        let mut ack = Ack::Ok;

        if request & REQ_RNW != 0 {
            // 讀：AP read 需先 post 一次再連續取回
            if request & REQ_APND_P != 0 {
                let (a, _) = self.transfer_retry(request, 0).await;
                ack = a;
                if a == Ack::Ok {
                    for i in 0..count {
                        let req_i = if i == count - 1 { DP_RDBUFF_RD } else { request };
                        let (a, val) = self.transfer_retry(req_i, 0).await;
                        ack = a;
                        if a != Ack::Ok {
                            break;
                        }
                        put_u32_le(&mut resp[ri..], val);
                        ri += 4;
                        processed += 1;
                    }
                }
            } else {
                for _ in 0..count {
                    let (a, val) = self.transfer_retry(request, 0).await;
                    ack = a;
                    if a != Ack::Ok {
                        break;
                    }
                    put_u32_le(&mut resp[ri..], val);
                    ri += 4;
                    processed += 1;
                }
            }
        } else {
            // 寫
            for _ in 0..count {
                let data = u32_le(&req[qi..qi + 4]);
                qi += 4;
                let (a, _) = self.transfer_retry(request, data).await;
                ack = a;
                if a != Ack::Ok {
                    break;
                }
                processed += 1;
            }
        }

        resp[1..3].copy_from_slice(&processed.to_le_bytes());
        resp[3] = ack.to_byte();
        ri
    }

    /// 帶 WAIT 重試的單筆 SWD transfer。
    pub(crate) async fn transfer_retry(&mut self, request: u8, wdata: u32) -> (Ack, u32) {
        let mut tries = self.retry_count as i32;
        loop {
            let (ack, val) = self.swd_transfer(request, wdata).await;
            if ack != Ack::Wait || tries <= 0 {
                return (ack, val);
            }
            tries -= 1;
        }
    }

    // ---------------- 低階 SWD 傳輸（對應 sw_dp_pio.c SWD_Transfer）----------------

    pub(crate) async fn swd_transfer(&mut self, request: u8, wdata: u32) -> (Ack, u32) {
        // 組請求封包：start(1) | A[..] | parity | stop(0) | park(1)
        let mut prq: u32 = 1 << 0; // start
        let mut parity = 0u32;
        for n in 0..4 {
            let bit = ((request >> n) & 1) as u32;
            prq |= bit << (n + 1);
            parity += bit;
        }
        prq |= (parity & 1) << 5; // parity
        prq |= 1 << 7; // park
        self.probe.write_bits(8, prq).await;

        // turnaround + 3-bit ACK
        let ackr = self.probe.read_bits(self.turnaround + 3).await;
        let ack = Ack::from_swd(((ackr >> self.turnaround) & 0x7) as u8);

        if ack == Ack::Ok {
            if request & REQ_RNW != 0 {
                // 讀資料相
                let val = self.probe.read_bits(32).await;
                let par = self.probe.read_bits(1).await;
                let mut a = Ack::Ok;
                if (val.count_ones() & 1) != (par & 1) {
                    a = Ack::Parity;
                }
                self.probe.hiz_clocks(self.turnaround).await; // line idle turnaround
                self.idle().await;
                return (a, val);
            } else {
                // 寫資料相
                self.probe.hiz_clocks(self.turnaround).await; // write turnaround
                self.probe.write_bits(32, wdata).await;
                self.probe.write_bits(1, wdata.count_ones() & 1).await;
                self.idle().await;
                return (Ack::Ok, 0);
            }
        }

        if ack == Ack::Wait || ack == Ack::Fault {
            if self.data_phase && (request & REQ_RNW != 0) {
                // dummy read 32+1
                self.clock_in_discard(33).await;
            }
            self.probe.hiz_clocks(self.turnaround).await;
            if self.data_phase && (request & REQ_RNW == 0) {
                self.probe.write_bits(32, 0).await;
                self.probe.write_bits(1, 0).await;
            }
            return (ack, 0);
        }

        // 協定錯誤：退避 turnaround + 32 + 1 位元
        self.clock_in_discard(self.turnaround + 32 + 1).await;
        (ack, 0)
    }

    pub(crate) async fn idle(&mut self) {
        // 最少 8 個 idle clock：host(OpenOCD/probe-rs)常把 idle_cycles 設 0、密集連發 AP 交易，
        // 長線在「讀資料 Hi-Z → 下一筆驅動」之間來不及 settle → AP 間歇失敗（DP 單筆卻沒事）。
        // 強制留 settle 時間，把邊際的密集 AP 序列拉穩。usbipd-rs 因單筆有間隔故原本就穩。
        let mut n = self.idle_cycles.max(8);
        while n > 0 {
            let c = n.min(256);
            self.probe.write_bits(c, 0).await;
            n -= c;
        }
    }

    async fn clock_in_discard(&mut self, mut n: u32) {
        while n > 0 {
            let c = n.min(32);
            let _ = self.probe.read_bits(c).await;
            n -= c;
        }
    }
}
