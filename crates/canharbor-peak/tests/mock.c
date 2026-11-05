/* SPDX-License-Identifier: AGPL-3.0-or-later */
#include "PeakUSB.h"
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
static unsigned opened[2], silent[2], nominal[2], data_rate[2], fd[2], queued[2], read_kind[2], status[2], empty_status[2], echo[2];
static unsigned writes, last_flags, fail_close, reject_silent, bad_readback, last_ret, last_dlc, bad_version;
static TPCANMsgFD messages[2];
static unsigned key(TPCANHandle h) { return h == 0x52 ? 1 : 0; }
void mock_reset(void) { memset(opened,0,sizeof(opened));memset(silent,0,sizeof(silent));memset(nominal,0,sizeof(nominal));memset(data_rate,0,sizeof(data_rate));memset(fd,0,sizeof(fd));memset(queued,0,sizeof(queued));memset(read_kind,0,sizeof(read_kind));memset(status,0,sizeof(status));memset(echo,0,sizeof(echo));memset(empty_status,0,sizeof(empty_status));writes=last_flags=fail_close=reject_silent=bad_readback=last_ret=last_dlc=bad_version=0; }
void mock_option(unsigned option,unsigned value) { if(option==1)fail_close=value;else if(option==2)reject_silent=value;else if(option==3)bad_readback=value;else if(option==4)last_ret=value;else if(option==5)bad_version=value; }
unsigned mock_writes(void){return writes;}
unsigned mock_flags(void){return last_flags;}
unsigned mock_dlc(void){return last_dlc;}
unsigned mock_open(unsigned h){return opened[key((TPCANHandle)h)];}
void mock_frame(unsigned h,unsigned flags,unsigned dlc) {unsigned k=key((TPCANHandle)h);memset(&messages[k],0,sizeof(messages[k]));messages[k].ID=0x654;messages[k].MSGTYPE=(BYTE)flags;messages[k].DLC=(BYTE)dlc;for(unsigned i=0;i<64;i++)messages[k].DATA[i]=(BYTE)i;queued[k]=1;}
void mock_identifier(unsigned h,unsigned id){messages[key((TPCANHandle)h)].ID=id;}
void mock_empty_status(unsigned h,unsigned value){empty_status[key(h)]=value;}
void mock_status(unsigned h,unsigned value){status[key((TPCANHandle)h)]=value;}
size_t mock_size(unsigned which){switch(which){case 0:return sizeof(TPCANMsg);case 1:return sizeof(TPCANTimestamp);case 2:return sizeof(TPCANMsgFD);default:return sizeof(TPCANChannelInformation);}}
size_t mock_offset(unsigned which){switch(which){case 0:return offsetof(TPCANMsg,DATA);case 1:return offsetof(TPCANTimestamp,micros);case 2:return offsetof(TPCANMsgFD,DATA);default:return offsetof(TPCANChannelInformation,device_id);}}
TPCANStatus CAN_GetValue(TPCANHandle h,TPCANParameter p,void *out,DWORD len){
 if(p==PCAN_API_VERSION){if(len<6)return PCAN_ERROR_ILLPARAMVAL;memcpy(out,"4.9.0",6);return 0;}
 if(p==PCAN_CHANNEL_VERSION && h==PCAN_NONEBUS){if(len<6)return PCAN_ERROR_ILLPARAMVAL;memcpy(out,bad_version?"9.9.9":"1.0.2",6);return 0;}
 if(p==PCAN_ATTACHED_CHANNELS_COUNT){if(len<4)return PCAN_ERROR_ILLPARAMVAL;*(DWORD*)out=2;return 0;}
 if(p==PCAN_ATTACHED_CHANNELS){if(len<2*sizeof(TPCANChannelInformation))return PCAN_ERROR_ILLPARAMVAL;TPCANChannelInformation *tab=out;memset(tab,0,2*sizeof(*tab));for(unsigned i=0;i<2;i++){tab[i].channel_handle=(WORD)(0x51+i);tab[i].device_type=PCAN_USB;tab[i].device_features=i;tab[i].channel_condition=opened[i]?PCAN_CHANNEL_OCCUPIED:PCAN_CHANNEL_AVAILABLE;snprintf(tab[i].device_name,sizeof(tab[i].device_name),"Synthetic PEAK %u",i);}return 0;}
 if(len<4)return PCAN_ERROR_ILLPARAMVAL;unsigned k=key(h);DWORD value=0;
 if(p==PCAN_LISTEN_ONLY)value=silent[k];else if(p==PCAN_BUSSPEED_NOMINAL)value=bad_readback?125000:nominal[k];else if(p==PCAN_BUSSPEED_FD)value=data_rate[k];else return PCAN_ERROR_ILLPARAMTYPE;memcpy(out,&value,4);return 0;
}
TPCANStatus CAN_SetValue(TPCANHandle h,TPCANParameter p,void *in,DWORD len){if(len!=4)return PCAN_ERROR_ILLPARAMVAL;DWORD value;memcpy(&value,in,4);unsigned k=key(h);if(p==PCAN_LISTEN_ONLY){if(reject_silent)return PCAN_ERROR_ILLOPERATION;silent[k]=value;}else if(p==PCAN_BUSOFF_AUTORESET){if(value)return PCAN_ERROR_ILLPARAMVAL;}else if(p==PCAN_ALLOW_ECHO_FRAMES)echo[k]=value;else return PCAN_ERROR_ILLPARAMTYPE;return 0;}
TPCANStatus CAN_Initialize(TPCANHandle h,TPCANBaudrate rate,TPCANType type,DWORD port,WORD interrupt){(void)type;(void)port;(void)interrupt;unsigned k=key(h);if(opened[k])return PCAN_ERROR_NETINUSE;if(rate!=PCAN_BAUD_500K)return PCAN_ERROR_ILLPARAMVAL;opened[k]=1;nominal[k]=500000;fd[k]=0;if(getenv("CANHARBOR_PEAK_TEST_RX")){mock_frame(h,0,2);}return 0;}
TPCANStatus CAN_InitializeFD(TPCANHandle h,TPCANBitrateFD text){unsigned k=key(h);if(k!=1||opened[k])return PCAN_ERROR_ILLOPERATION;unsigned clock=0,nb=0,n1=0,n2=0,ns=0,db=0,d1=0,d2=0,ds=0;
 if(sscanf(text,"f_clock=%u,nom_brp=%u,nom_tseg1=%u,nom_tseg2=%u,nom_sjw=%u,data_brp=%u,data_tseg1=%u,data_tseg2=%u,data_sjw=%u",&clock,&nb,&n1,&n2,&ns,&db,&d1,&d2,&ds)!=9||!nb||!db)return PCAN_ERROR_ILLPARAMVAL;opened[k]=1;fd[k]=1;nominal[k]=clock/(nb*(1+n1+n2));data_rate[k]=clock/(db*(1+d1+d2));if(getenv("CANHARBOR_PEAK_TEST_RX")){mock_frame(h,0,2);}return 0;}
TPCANStatus CAN_Uninitialize(TPCANHandle h){unsigned k=key(h);if(fail_close)return PCAN_ERROR_RESOURCE;opened[k]=0;queued[k]=0;return 0;}
TPCANStatus CAN_GetStatus(TPCANHandle h){return status[key(h)];}
TPCANStatus CAN_ReadFD(TPCANHandle h,TPCANMsgFD *out,TPCANTimestampFD *stamp){unsigned k=key(h);if(!opened[k]||!fd[k])return PCAN_ERROR_INITIALIZE;if(!queued[k]){unsigned result=PCAN_ERROR_QRCVEMPTY|status[k]|empty_status[k];empty_status[k]=0;return result;}*out=messages[k];*stamp=0x123456789ULL;queued[k]=0;return status[k];}
TPCANStatus CAN_Read(TPCANHandle h,TPCANMsg *out,TPCANTimestamp *stamp){unsigned k=key(h);if(!opened[k]||fd[k])return PCAN_ERROR_INITIALIZE;if(!queued[k]){unsigned result=PCAN_ERROR_QRCVEMPTY|status[k]|empty_status[k];empty_status[k]=0;return result;}out->ID=messages[k].ID;out->MSGTYPE=messages[k].MSGTYPE;out->LEN=messages[k].DLC;memcpy(out->DATA,messages[k].DATA,8);stamp->millis=123;stamp->millis_overflow=1;stamp->micros=456;queued[k]=0;return status[k];}
TPCANStatus CAN_WriteFD(TPCANHandle h,TPCANMsgFD *message){unsigned k=key(h);if(!opened[k]||silent[k]||!fd[k])return PCAN_ERROR_ILLMODE;if(last_ret)return last_ret;writes++;last_flags=message->MSGTYPE;last_dlc=message->DLC;if(echo[k] && k==1){messages[k]=*message;messages[k].MSGTYPE|=PCAN_MESSAGE_ECHO;queued[k]=1;}return 0;}
TPCANStatus CAN_Write(TPCANHandle h,TPCANMsg *message){unsigned k=key(h);if(!opened[k]||silent[k]||fd[k])return PCAN_ERROR_ILLMODE;if(last_ret)return last_ret;writes++;last_flags=message->MSGTYPE;last_dlc=message->LEN;if(echo[k] && k==1){memset(&messages[k],0,sizeof(messages[k]));messages[k].ID=message->ID;messages[k].MSGTYPE=message->MSGTYPE|PCAN_MESSAGE_ECHO;messages[k].DLC=message->LEN;memcpy(messages[k].DATA,message->DATA,8);queued[k]=1;}return 0;}
