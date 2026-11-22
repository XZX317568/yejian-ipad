//! Optional local-network progress exchange using the existing mutual-TLS host.
use std::{path::PathBuf,sync::{Arc,mpsc},net::{IpAddr,Ipv4Addr},time::Duration};
use egui::*;
use mangayomi_sync_host::{Identity,Host,HostConfig,Handler,HostResponse,RequestKind,PeerClient};
use serde::{Serialize,Deserialize};
use yejian_core::{Library,ProgressRecord};

#[derive(Serialize,Deserialize)]
struct IdentityFile { key:String,certificate:String }
#[derive(Serialize,Deserialize)]
struct Packet { version:u32,records:Vec<ProgressRecord>,#[serde(default)] skipped:usize }
enum Task { Start, Stop, Connect{url:String,fingerprint:String,code:String} }
enum Reply { Hosting{port:u16,fingerprint:String,code:String}, Stopped, Status(String), Changed, Error(String) }

pub struct ProgressSync {
    tx:mpsc::Sender<Task>,rx:mpsc::Receiver<Reply>,
    hosting:bool,busy:bool,url:String,fingerprint:String,code:String,
    invitation:Option<(u16,String,String)>,status:String,
    pub changed:bool,
}
impl ProgressSync {
    pub fn new(root:PathBuf,ctx:Context)->Self {
        Self::with_address(root,ctx,IpAddr::V4(Ipv4Addr::UNSPECIFIED))
    }
    fn with_address(root:PathBuf,ctx:Context,address:IpAddr)->Self {
        let(tx,rx_task)=mpsc::channel();let(tx_reply,rx)=mpsc::channel();
        crate::diagnostics::spawn_supervised("yejian-sync",true,||crate::diagnostics::report_recovery("设备间同步"),move||{
            let (tx_reply,ctx)=(tx_reply.clone(),ctx.clone());
            let notify=Arc::new(move|reply|{let _=tx_reply.send(reply);ctx.request_repaint();});
            let runtime=match tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build(){Ok(r)=>r,Err(e)=>{notify(Reply::Error(e.to_string()));return;}};
            let mut host:Option<Host>=None;
            while let Ok(task)=rx_task.recv(){
                let result=(||->anyhow::Result<()> {
                    match task {
                        Task::Stop=>{
                            if let Some(host)=host.take(){runtime.block_on(host.stop());}
                            notify(Reply::Stopped);
                        }
                        Task::Start=>{
                            if host.is_some(){return Ok(());}
                            let identity=identity(&root)?;
                            let library=Arc::new(Library::open(&root)?);
                            let handler_library=library.clone();
                            let handler_notify=notify.clone();
                            let handler:Handler=Arc::new(move|request|{
                                let library=handler_library.clone();
                                let notify=handler_notify.clone();
                                Box::pin(async move{
                                    let result=(||->anyhow::Result<Vec<u8>>{
                                        if matches!(request.kind,RequestKind::Pair{..}){return Ok(b"{}".to_vec());}
                                        let packet:Packet=serde_json::from_slice(&request.body)?;
                                        anyhow::ensure!(packet.version==1,"不兼容的同步协议");
                                        anyhow::ensure!(packet.records.len()<=20000,"一次同步最多20000条");
                                        let report=library.apply_progress(&packet.records)?;
                                        notify(Reply::Changed);
                                        Ok(serde_json::to_vec(&Packet{version:1,records:library.export_progress()?,skipped:report.skipped_count()})?)
                                    })();
                                    match result{
                                        Ok(body)=>HostResponse{status:200,body,approve_peer:matches!(request.kind,RequestKind::Pair{..})},
                                        Err(e)=>HostResponse{status:400,body:serde_json::to_vec(&serde_json::json!({"error":e.to_string()})).unwrap_or_default(),approve_peer:false},
                                    }
                                })
                            });
                            let new_host=runtime.block_on(Host::start(HostConfig{identity:identity.clone(),address,port:0,paired:vec![],allow_public_networks:false,max_body_bytes:8*1024*1024,response_timeout:Duration::from_secs(30)},handler))?;
                            // A fresh random certificate fingerprint provides sufficient entropy
                            // for a short-lived, one-use pairing code without a second RNG.
                            let secret=Identity::generate()?.fingerprint;
                            let code=format!("{}-{}-{}",&secret[0..4],&secret[4..8],&secret[8..12]);
                            new_host.open_pairing(&code,Duration::from_secs(300))?;
                            notify(Reply::Hosting{port:new_host.local_addr().port(),fingerprint:identity.fingerprint,code});
                            host=Some(new_host);
                        }
                        Task::Connect{url,fingerprint,code}=>{
                            let library=Library::open(&root)?;
                            let identity=identity(&root)?;
                            let client=PeerClient::new(&identity,&url,&fingerprint,Duration::from_secs(30))?;
                            let paired=runtime.block_on(client.pair(&code,"页间设备"))?;
                            anyhow::ensure!(paired.status==200,"配对未完成（HTTP {}），检查配对码和有效期",paired.status);
                            let body=serde_json::to_vec(&Packet{version:1,records:library.export_progress()?,skipped:0})?;
                            let response=runtime.block_on(client.sync(body))?;
                            anyhow::ensure!(response.status==200,"同步未完成（HTTP {}）",response.status);
                            let packet:Packet=serde_json::from_slice(&response.body)?;
                            anyhow::ensure!(packet.version==1,"不兼容的同步协议");
                            let report=library.apply_progress(&packet.records)?;
                            notify(Reply::Status(format!("同步完成：本机更新 {} 本，保留 {} 本，未应用 {} 条；另一台设备未应用 {} 条。缺少漫画文件时先导入同一文件再同步。",report.applied_count(),report.unchanged_count(),report.skipped_count(),packet.skipped)));
                            notify(Reply::Changed);
                        }
                    }Ok(())
                })();
                if let Err(e)=result{notify(Reply::Error(format!("{e:#}")));}
            }
            if let Some(host)=host{runtime.block_on(host.stop());}
        });
        Self{tx,rx,hosting:false,busy:false,url:String::new(),fingerprint:String::new(),code:String::new(),invitation:None,status:String::new(),changed:false}
    }
    pub fn poll(&mut self){
        while let Ok(reply)=self.rx.try_recv(){
            self.busy=false;
            match reply{
                Reply::Hosting{port,fingerprint,code}=>{self.hosting=true;self.invitation=Some((port,fingerprint,code));self.status="等待另一台设备，配对码五分钟内有效".into();}
                Reply::Stopped=>{self.hosting=false;self.invitation=None;self.status="已停止接收".into();self.changed=true;}
                Reply::Status(text)=>self.status=text,
                Reply::Changed=>self.changed=true,
                Reply::Error(text)=>self.status=format!("同步失败：{text}"),
            }
        }
    }
    pub fn ui(&mut self,ui:&mut Ui){
        self.poll();
        ui.heading("双端续读");
        ui.label("两台设备连接同一网络，并各自导入相同漫画文件。同步阅读进度和分类。");
        ui.add_enabled_ui(!self.busy,|ui|{
            if self.hosting{
                if ui.button("停止接收并刷新书库").clicked(){let _=self.tx.send(Task::Stop);self.busy=true;}
            }else if ui.button("在这台设备接收同步").clicked(){let _=self.tx.send(Task::Start);self.busy=true;}
            if let Some((port,fingerprint,code))=&self.invitation{
                ui.label(format!("端口 {port} · 配对码 {code}"));
                ui.label("在另一台设备填写本机局域网 IP、此端口和下方指纹。");
                ui.add(Label::new(RichText::new(fingerprint).monospace().size(10.)).wrap());
                if ui.button("复制指纹").clicked(){ui.ctx().copy_text(fingerprint.clone());}
            }
            ui.add_space(12.);
            ui.add(TextEdit::singleline(&mut self.url).hint_text("另一台设备地址：https://192.168.…:端口"));
            ui.add(TextEdit::singleline(&mut self.fingerprint).hint_text("另一台设备的完整指纹"));
            ui.add(TextEdit::singleline(&mut self.code).hint_text("配对码"));
            if ui.button("连接并同步").clicked(){
                let _=self.tx.send(Task::Connect{url:self.url.trim().into(),fingerprint:self.fingerprint.trim().into(),code:self.code.trim().into()});self.busy=true;
            }
        });
        if !self.status.is_empty(){ui.label(&self.status);}
    }
}
fn identity(root:&std::path::Path)->anyhow::Result<Identity>{
    let path=root.join("device-identity.json");
    if path.exists(){let stored:IdentityFile=serde_json::from_slice(&std::fs::read(path)?)?;return Identity::from_pem(&stored.key,&stored.certificate);}
    std::fs::create_dir_all(root)?;
    let identity=Identity::generate()?;
    let content=serde_json::to_vec(&IdentityFile{key:identity.private_key_pem.clone(),certificate:identity.certificate_pem.clone()})?;
    use std::io::Write;
    let mut file=std::fs::OpenOptions::new().create_new(true).write(true).open(&path)?;
    file.write_all(&content)?;file.sync_all()?;
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn paired_libraries_exchange_real_progress_over_mutual_tls() {
        let temp=tempfile::tempdir().unwrap();
        let file=temp.path().join("same.cbz");
        let mut archive=zip::ZipWriter::new(std::fs::File::create(&file).unwrap());
        let mut png=std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(8,8,image::Rgb([55,88,100])).write_to(&mut png,image::ImageFormat::Png).unwrap();
        for page in 0..6 {
            archive.start_file(format!("{page}.png"),zip::write::SimpleFileOptions::default()).unwrap();
            archive.write_all(png.get_ref()).unwrap();
        }
        archive.finish().unwrap();
        let left_root=temp.path().join("left");let right_root=temp.path().join("right");
        let left=Library::open(&left_root).unwrap();let book=left.import(&file).unwrap();
        left.save_progress(&book.id,4,false).unwrap();
        let right=Library::open(&right_root).unwrap();right.import(&file).unwrap();
        let mut host=ProgressSync::with_address(left_root,Context::default(),IpAddr::V4(Ipv4Addr::LOCALHOST));
        host.tx.send(Task::Start).unwrap();
        let deadline=std::time::Instant::now()+Duration::from_secs(15);
        while host.invitation.is_none(){
            host.poll();assert!(!host.status.starts_with("同步失败"),"{}",host.status);
            assert!(std::time::Instant::now()<deadline);std::thread::sleep(Duration::from_millis(10));
        }
        let(port,fingerprint,code)=host.invitation.clone().unwrap();
        let mut guest=ProgressSync::new(right_root,Context::default());
        guest.tx.send(Task::Connect{url:format!("https://127.0.0.1:{port}"),fingerprint,code}).unwrap();
        while !guest.changed {
            guest.poll();assert!(!guest.status.starts_with("同步失败"),"{}",guest.status);
            assert!(std::time::Instant::now()<deadline);std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(left.get_book(&book.id).unwrap().current_page,4);
        assert_eq!(right.get_book(&book.id).unwrap().current_page,4);
        host.tx.send(Task::Stop).unwrap();
    }
}
